//! Krark's Thumb with two or more copies under one controller — CR 616.1
//! commuting coin-flip count multipliers.
//!
//! Oracle (verbatim, card-data.json): "If you would flip a coin, instead flip
//! two coins and ignore one." Two copies under one controller are two applicable
//! replacement effects on the SAME `CoinFlip` event (CR 616.1), but each is a
//! pure count multiplier — ×2 then ×2 (or the reverse) takes the count 1 → 2 → 4,
//! and CR 614.5 applies each exactly once (CR 616.1f repeats until none remain) — so the ordering choice is immaterial
//! and `candidate_materiality` must classify both as commuting `Count` writers
//! for `pipeline_loop` to auto-apply them. Before that classification the
//! pipeline parked a CR 616.1 `ReplacementChoice` (a debug resolution-invariant
//! panic) and then dropped the flip entirely: no coin was flipped and no keep
//! prompt ever appeared.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 614.1a: effects that use the word "instead" are replacement effects.
//!   - CR 614.5: a replacement effect gets one opportunity to affect an event.
//!   - CR 614.6: a replaced event never happens (the ignored flips don't occur).
//!   - CR 616.1 / CR 616.1f: the affected player orders two or more applicable
//!     replacements, and the process repeats until none are left to apply.
//!   - CR 705.1: flipping a coin; CR 705.2: only the flipper wins or loses it.
//!   - CR 205.4d + CR 704.5j: the legendary supertype and the legend rule, which
//!     Mirror Gallery's "The \"legend rule\" doesn't apply." switches off.
//!
//! These are runtime tests through the real pipeline (`GameAction::CastSpell` →
//! CR 614 replacement pipeline → RNG → keep choice). `runner.cast(..).resolve()`
//! is deliberately NOT used here: the scenario driver auto-answers
//! `CoinFlipKeepChoice`, which is exactly the prompt under test.

use engine::game::replacement::{replace_event, ReplacementResult};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, CoinFlipRun, Effect, QuantityExpr, QuantityRef,
    ReplacementDefinition, ReplacementPlayerScope, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, GameRng, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;
use std::collections::HashSet;

/// Krark's Thumb printed Oracle text — byte-identical to card-data.json.
const KRARKS_THUMB: &str = "If you would flip a coin, instead flip two coins and ignore one.";
/// Mirror Gallery printed Oracle text (CR 704.5j exemption).
const MIRROR_GALLERY: &str = "The \"legend rule\" doesn't apply.";
/// Puppet's Verdict printed Oracle text: one flip whose win/lose branches destroy
/// disjoint creature sets (power ≤ 2 vs power ≥ 3), so which result is kept is
/// observable on the board.
const PUPPETS_VERDICT: &str = "Flip a coin. If you win the flip, destroy all creatures with power 2 or less. If you lose the flip, destroy all creatures with power 3 or greater.";
/// Flock of Rabid Sheep: an N-flip instruction with one independent win branch
/// per flip (CR 705.2), so N logical flips must open N keep prompts.
const FLOCK_OF_RABID_SHEEP: &str = "Flip X coins. For each flip you win, create a 2/2 green Sheep creature token named Rabid Sheep.";
/// Crazed Firecat: a flip-until-lose run, one +1/+1 counter per won flip.
const CRAZED_FIRECAT: &str = "When this creature enters, flip a coin until you lose a flip. Put a +1/+1 counter on this creature for each flip you won.";

// --- Fixtures ---------------------------------------------------------------

/// CR 205.4d + CR 704.5j: a LEGENDARY Krark's Thumb (its printed supertype), so
/// N copies only coexist because Mirror Gallery switches the legend rule off.
fn add_thumb(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_artifact_from_oracle(P0, "Krark's Thumb", KRARKS_THUMB)
        .as_legendary()
        .id()
}

/// CR 205.4a: `controller`'s legendary Krark's Thumb.
fn add_thumb_for(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    scenario
        .add_artifact_from_oracle(controller, "Krark's Thumb", KRARKS_THUMB)
        .as_legendary()
        .id()
}

/// CR 704.5j: "The \"legend rule\" doesn't apply."
fn add_gallery(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_artifact_from_oracle(P0, "Mirror Gallery", MIRROR_GALLERY)
        .id()
}

/// A scaffold permanent carrying synthetic replacement definitions (T5/T6):
/// the pipeline reads definitions off any permanent, so a 0/1 body is enough.
fn add_synthetic_source(scenario: &mut GameScenario, defs: Vec<ReplacementDefinition>) {
    let mut builder = scenario.add_creature(P0, "Synthetic Source", 0, 1);
    for def in defs {
        builder.with_replacement_definition(def);
    }
}

/// A synthetic `CoinFlip` replacement whose execute rescales the event's own
/// count by `factor` — the shape `event_count_multiplier_commute_class` must
/// classify as `Multiplicative`, controller-scoped like Krark's Thumb.
fn factor_coin_flip_replacement(factor: i32) -> ReplacementDefinition {
    let mut def =
        ReplacementDefinition::new(ReplacementEvent::CoinFlip).execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::Exactly {
                    count: QuantityExpr::Multiply {
                        factor,
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::EventContextAmount,
                        }),
                    },
                },
                win_effect: None,
                lose_effect: None,
                flipper: TargetFilter::Controller,
            },
        ));
    def.valid_player = Some(ReplacementPlayerScope::You);
    def
}

/// A synthetic `CoinFlip` replacement whose execute OFFSETS the count ("flip one
/// more coin") — not a multiplier, so it must stay order-material (T6).
fn offset_coin_flip_replacement() -> ReplacementDefinition {
    let mut def =
        ReplacementDefinition::new(ReplacementEvent::CoinFlip).execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoins {
                flips: CoinFlipRun::Exactly {
                    count: QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Ref {
                            qty: QuantityRef::EventContextAmount,
                        }),
                        offset: 1,
                    },
                },
                win_effect: None,
                lose_effect: None,
                flipper: TargetFilter::Controller,
            },
        ));
    def.valid_player = Some(ReplacementPlayerScope::You);
    def
}

// --- Manual driver ----------------------------------------------------------

/// The outcome of driving one interactive coin-flip resolution by hand.
struct FlipDrive {
    /// `results` of every keep prompt, in the order the prompts were answered.
    prompts: Vec<Vec<bool>>,
    /// The result kept at each prompt (`prompts[i][kept_index]`), in order.
    kept: Vec<bool>,
    /// Every event reported by the actions this driver ran.
    events: Vec<GameEvent>,
}

/// CR 601.2: put `spell` on the stack through the real action path and settle the
/// announcement prompts (X, mana payment). Any other window panics — a prompt
/// before resolution can start means the fixture, not the flip, is under test.
fn cast_spell(runner: &mut GameRunner, spell: ObjectId, x: Option<u32>) {
    let card_id = runner.state().objects[&spell].card_id;
    let mut result = runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("the cast action must be accepted");
    for _ in 0..16 {
        match &result.waiting_for {
            // CR 601.2b: X (a variable cost) is announced while proposing the spell.
            WaitingFor::ChooseXValue { .. } => {
                let value = x.expect("this spell announces an X value");
                result = runner
                    .act(GameAction::ChooseX { value })
                    .expect("announcing X must succeed");
            }
            // CR 601.2h: finalize cost payment (the fixture's pool covers it).
            WaitingFor::ManaPayment { .. } => {
                result = runner
                    .act(GameAction::PassPriority)
                    .expect("finalizing a pool-funded payment must succeed");
            }
            _ => return,
        }
    }
    panic!("the cast's announcement prompts did not settle within 16 steps");
}

/// Step the state machine until the flipper must keep a coin result and return
/// the offered results plus every event the steps reported — so a caller can
/// assert that no flip has "happened" yet (CR 614.6).
///
/// The prompt is asserted here, unconditionally, as the positive reach guard for
/// the negative assertions in the callers: anything other than the required
/// `CoinFlipKeepChoice { player: P0, keep_count: 1 }` — including the
/// `ReplacementChoice` park this change removes — panics.
fn pause_at_keep(runner: &mut GameRunner) -> (Vec<bool>, Vec<GameEvent>) {
    let mut events = Vec::new();
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::CoinFlipKeepChoice {
                player,
                results,
                keep_count,
            } => {
                assert_eq!(player, P0, "the flipper keeps the result (CR 705.2)");
                assert_eq!(keep_count, 1, "Krark's Thumb ignores all but one flip");
                assert!(
                    runner.state().active_coin_flip_frame().is_some(),
                    "the typed coin-flip frame must own the keep prompt"
                );
                return (results, events);
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                panic!("the resolution finished without offering a keep choice")
            }
            WaitingFor::Priority { .. } => {
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority toward the flip must succeed");
                events.extend(result.events);
            }
            // CR 603.3b: order this controller's pending triggers (identity order
            // — one trigger in these fixtures).
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                let result = runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering the pending triggers must succeed");
                events.extend(result.events);
            }
            WaitingFor::ReplacementChoice { .. } => panic!(
                "a coin-flip replacement parked a CR 616.1 ordering choice: two commuting \
                 count multipliers must auto-apply (this is the pre-fix defect)"
            ),
            other => panic!("unexpected window before the keep choice: {other:?}"),
        }
    }
    panic!("no keep prompt appeared within 64 steps");
}

/// Answer every `CoinFlipKeepChoice` with `choose(prompt_index, results)`,
/// passing priority, until the stack is empty at `Priority`. Same unconditional
/// prompt guards as [`pause_at_keep`].
fn drive_coin_flips(
    runner: &mut GameRunner,
    mut choose: impl FnMut(usize, &[bool]) -> usize,
) -> FlipDrive {
    let mut drive = FlipDrive {
        prompts: Vec::new(),
        kept: Vec::new(),
        events: Vec::new(),
    };
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::CoinFlipKeepChoice {
                player,
                results,
                keep_count,
            } => {
                assert_eq!(player, P0, "the flipper keeps the result (CR 705.2)");
                assert_eq!(keep_count, 1, "Krark's Thumb ignores all but one flip");
                let index = choose(drive.prompts.len(), &results);
                assert!(
                    index < results.len(),
                    "the keep index must address the offered results"
                );
                let result = runner
                    .act(GameAction::SelectCoinFlips {
                        keep_indices: vec![index],
                    })
                    .expect("SelectCoinFlips must succeed");
                drive.kept.push(results[index]);
                drive.prompts.push(results);
                drive.events.extend(result.events);
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return drive,
            WaitingFor::Priority { .. } => {
                let result = runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority must succeed");
                drive.events.extend(result.events);
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                let result = runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("ordering the pending triggers must succeed");
                drive.events.extend(result.events);
            }
            WaitingFor::ReplacementChoice { .. } => panic!(
                "a coin-flip replacement parked a CR 616.1 ordering choice: two commuting \
                 count multipliers must auto-apply (this is the pre-fix defect)"
            ),
            other => panic!("unexpected waiting state driving coin flips: {other:?}"),
        }
    }
    panic!("the coin-flip drive did not reach an empty stack within 64 steps");
}

/// The `won` value of every `CoinFlipped` this player produced, in order
/// (CR 705.2: only the flipper wins or loses the flip).
fn coin_flips_by(events: &[GameEvent], player: PlayerId) -> Vec<bool> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CoinFlipped { player_id, won } if *player_id == player => Some(*won),
            _ => None,
        })
        .collect()
}

/// Battlefield permanents P0 controls with this name.
fn p0_count(runner: &GameRunner, name: &str) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter(|id| state.objects[id].name == name)
        .count()
}

/// The `+1/+1` counters on `object`.
fn plus_one_counters(runner: &GameRunner, object: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&object)
        .and_then(|object| object.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn green_mana(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![]))
        .collect()
}

// --- T1: a single flip under two Thumbs -------------------------------------

/// Two legendary Thumbs plus Mirror Gallery, P1 holding a 2/2 and a 4/4, and
/// Puppet's Verdict in P0's hand. Puppet's Verdict is an instant with no mana cost
/// set on the fixture, so the cast needs no mana.
fn t1_fixture(seed: u64) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_gallery(&mut scenario);
    add_thumb(&mut scenario);
    add_thumb(&mut scenario);
    let small = scenario.add_creature(P1, "Small Bear", 2, 2).id();
    let big = scenario.add_creature(P1, "Big Dreadmaw", 4, 4).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Puppet's Verdict", true, PUPPETS_VERDICT)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().rng = GameRng::seed_from_u64(seed);
    (runner, spell, small, big)
}

/// A seed whose FIRST Puppet's Verdict keep prompt offers four flips holding both
/// a win and a loss — so which result is kept changes which creature dies and
/// the assertion cannot pass vacuously. Also proves the fixture's legend-rule
/// exemption is real: two legendary Thumbs coexist.
fn t1_seed_with_mixed_prompt() -> u64 {
    for seed in 0..64u64 {
        let (mut runner, spell, _, _) = t1_fixture(seed);
        assert_eq!(
            p0_count(&runner, "Krark's Thumb"),
            2,
            "Mirror Gallery must keep both legendary Thumbs on the battlefield (CR 704.5j)"
        );
        cast_spell(&mut runner, spell, None);
        let drive = drive_coin_flips(&mut runner, |_, _| 0);
        if drive
            .prompts
            .first()
            .is_some_and(|results| results.contains(&true) && results.contains(&false))
        {
            return seed;
        }
    }
    panic!(
        "no seed in 0..64 opened a mixed four-coin keep prompt for Puppet's Verdict under two Thumbs"
    );
}

/// Two Thumbs scale a single flip to 4 coins and open exactly ONE keep prompt,
/// the kept result runs the matching branch, exactly one `CoinFlipped` happens,
/// and the resolution rests at `Priority` with no stale coin-flip frame.
#[test]
fn two_thumbs_single_flip_offers_one_keep_choice_over_four_coins() {
    let seed = t1_seed_with_mixed_prompt();
    let (mut runner, spell, small, big) = t1_fixture(seed);
    cast_spell(&mut runner, spell, None);

    let (results, pre_prompt_events) = pause_at_keep(&mut runner);
    assert_eq!(
        results.len(),
        4,
        "two ×2 multipliers take one flip to 2 × 2 = 4 coins (CR 616.1f)"
    );
    // CR 614.6: the coins are flipped but the prompt is unanswered, so no flip
    // has "happened" yet — the ignored flips never occur.
    assert!(
        coin_flips_by(&pre_prompt_events, P0).is_empty(),
        "no CoinFlipped may be emitted while the keep choice is unanswered"
    );
    assert!(
        results.contains(&true) && results.contains(&false),
        "the seed must offer a discriminating keep, got {results:?}"
    );
    assert!(
        runner.state().active_coin_flip_frame().is_some(),
        "the keep prompt is owned by the typed coin-flip frame"
    );

    // Keep a LOSS: Puppet's Verdict destroys every creature with power 3 or greater.
    let loss = results
        .iter()
        .position(|won| !*won)
        .expect("a loss is offered");
    let drive = drive_coin_flips(&mut runner, |_, _| loss);

    assert_eq!(
        drive.prompts.len(),
        1,
        "a single logical flip opens exactly one keep prompt"
    );
    // CR 614.6: only the kept flip happens — exactly one CoinFlipped, holding the
    // kept result (the ignored flips never occur).
    assert_eq!(
        coin_flips_by(&drive.events, P0),
        vec![false],
        "exactly one CoinFlipped, reporting the kept loss"
    );

    let state = runner.state();
    assert!(
        !state.battlefield.contains(&big),
        "the 4/4 dies on a lost flip (power ≥ 3 branch)"
    );
    assert_eq!(
        state.objects[&big].zone,
        Zone::Graveyard,
        "the destroyed 4/4 is in the graveyard"
    );
    assert!(
        state.battlefield.contains(&small),
        "the 2/2 survives a lost flip"
    );
    assert_eq!(
        p0_count(&runner, "Krark's Thumb"),
        2,
        "both Thumbs survive the resolution (reach guard: no legend-rule death)"
    );
    assert_eq!(
        p0_count(&runner, "Mirror Gallery"),
        1,
        "Mirror Gallery stays on the battlefield"
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty(), "the spell finished resolving");
    assert!(
        state.active_coin_flip_frame().is_none(),
        "no stale coin-flip frame is left behind"
    );
}

/// The same board, keeping a WIN: the 2/2 dies and the 4/4 survives — proving
/// `results[i]` (not the first coin) drives the branch.
#[test]
fn two_thumbs_single_flip_keeps_a_win_and_runs_the_win_branch() {
    let seed = t1_seed_with_mixed_prompt();
    let (mut runner, spell, small, big) = t1_fixture(seed);
    cast_spell(&mut runner, spell, None);

    let (results, _) = pause_at_keep(&mut runner);
    assert_eq!(results.len(), 4, "two Thumbs scale one flip to four coins");
    let win = results
        .iter()
        .position(|won| *won)
        .expect("the seed offers a win");
    let drive = drive_coin_flips(&mut runner, |_, _| win);

    assert_eq!(drive.prompts.len(), 1, "one keep prompt per logical flip");
    assert_eq!(
        coin_flips_by(&drive.events, P0),
        vec![true],
        "exactly one CoinFlipped, reporting the kept win"
    );

    let state = runner.state();
    assert!(
        !state.battlefield.contains(&small),
        "the 2/2 dies on a won flip (power ≤ 2 branch)"
    );
    assert_eq!(state.objects[&small].zone, Zone::Graveyard);
    assert!(
        state.battlefield.contains(&big),
        "the 4/4 survives a won flip"
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.active_coin_flip_frame().is_none());
}

// --- T2: an N-flip instruction under two Thumbs -----------------------------

/// Two Thumbs plus Mirror Gallery, Flock of Rabid Sheep (X = 2) in hand with its
/// real {X}{G}{G} cost funded from the pool.
fn t2_fixture(seed: u64) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_gallery(&mut scenario);
    add_thumb(&mut scenario);
    add_thumb(&mut scenario);
    scenario.with_mana_pool(P0, green_mana(4));
    let spell = {
        let mut builder = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Flock of Rabid Sheep",
            false,
            FLOCK_OF_RABID_SHEEP,
        );
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green, ManaCostShard::Green],
            generic: 0,
        });
        builder.id()
    };
    let mut runner = scenario.build();
    runner.state_mut().rng = GameRng::seed_from_u64(seed);
    (runner, spell)
}

/// Cast Flock of Rabid Sheep with X = 2 and keep a win whenever one is offered.
fn t2_run(seed: u64) -> (GameRunner, FlipDrive) {
    let (mut runner, spell) = t2_fixture(seed);
    cast_spell(&mut runner, spell, Some(2));
    let drive = drive_coin_flips(&mut runner, |_, results| {
        results.iter().position(|won| *won).unwrap_or(0)
    });
    (runner, drive)
}

/// A seed whose run keeps at least one win, so the token assertion cannot pass
/// vacuously.
fn t2_seed_with_a_kept_win() -> u64 {
    for seed in 0..64u64 {
        let (_, drive) = t2_run(seed);
        if drive.prompts.len() == 2 && drive.kept.contains(&true) {
            return seed;
        }
    }
    panic!("no seed in 0..64 kept a win inside two four-coin prompts");
}

/// Krark's Thumb 2019-01-25 ruling: each coin of "flip X coins" is replaced separately, so X = 2 under
/// two Thumbs opens two independent keep prompts of four coins, and the win
/// branch runs once per kept win.
#[test]
fn two_thumbs_flip_x_prompts_once_per_coin_over_four_coins() {
    let seed = t2_seed_with_a_kept_win();
    let (runner, drive) = t2_run(seed);

    assert_eq!(
        drive.prompts.len(),
        2,
        "X = 2 is two logical flips, so two keep prompts"
    );
    for (index, results) in drive.prompts.iter().enumerate() {
        assert_eq!(
            results.len(),
            4,
            "prompt {index} must offer 2 × 2 = 4 coins under two Thumbs, got {results:?}"
        );
    }
    let kept_wins = drive.kept.iter().filter(|won| **won).count();
    assert!(
        kept_wins >= 1,
        "the fixture seed must keep at least one win, kept: {:?}",
        drive.kept
    );
    // CR 614.6: one CoinFlipped per logical flip, holding that flip's kept result.
    assert_eq!(
        coin_flips_by(&drive.events, P0),
        drive.kept,
        "one CoinFlipped per keep prompt, each reporting the kept result"
    );

    let state = runner.state();
    let sheep = state
        .battlefield
        .iter()
        .filter(|id| {
            let object = &state.objects[id];
            object.is_token && object.controller == P0 && object.name == "Rabid Sheep"
        })
        .count();
    assert_eq!(
        sheep, kept_wins,
        "one Rabid Sheep token per won flip (kept wins: {kept_wins})"
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty(), "the spell finished resolving");
    assert!(state.active_coin_flip_frame().is_none());
}

// --- T3: a flip-until-lose run under two Thumbs -----------------------------

fn t3_fixture(seed: u64) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_gallery(&mut scenario);
    add_thumb(&mut scenario);
    add_thumb(&mut scenario);
    let firecat = scenario
        .add_creature_to_hand_from_oracle(P0, "Crazed Firecat", 4, 4, CRAZED_FIRECAT)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().rng = GameRng::seed_from_u64(seed);
    (runner, firecat)
}

/// Cast Crazed Firecat, keep a win on the first prompt and a loss on every later
/// one (so the run ends on the second prompt).
fn t3_run(seed: u64) -> (GameRunner, ObjectId, FlipDrive) {
    let (mut runner, firecat) = t3_fixture(seed);
    cast_spell(&mut runner, firecat, None);
    let drive = drive_coin_flips(&mut runner, |index, results| {
        if index == 0 {
            results.iter().position(|won| *won).unwrap_or(0)
        } else {
            results.iter().position(|won| !*won).unwrap_or(0)
        }
    });
    (runner, firecat, drive)
}

/// A seed whose first prompt offers a win and whose second offers a loss — the
/// mixed-result precondition that makes the kept results discriminating.
fn t3_seed_with_win_then_loss() -> u64 {
    for seed in 0..64u64 {
        let (_, _, drive) = t3_run(seed);
        if drive.prompts.len() == 2
            && drive.prompts[0].contains(&true)
            && drive.prompts[1].contains(&false)
        {
            return seed;
        }
    }
    panic!("no seed in 0..64 flipped a win then offered a loss for Crazed Firecat");
}

/// Every flip of an until-lose run is replaced separately, and the run stops on
/// the first KEPT loss: a kept win then a kept loss gives exactly one +1/+1
/// counter and two `CoinFlipped` events.
#[test]
fn two_thumbs_flip_until_lose_prompts_four_coins_per_flip() {
    let seed = t3_seed_with_win_then_loss();
    let (runner, firecat, drive) = t3_run(seed);

    assert_eq!(
        drive.prompts.len(),
        2,
        "the run ends on the first kept loss (CR 705.2)"
    );
    for (index, results) in drive.prompts.iter().enumerate() {
        assert_eq!(
            results.len(),
            4,
            "prompt {index} must offer 2 × 2 = 4 coins under two Thumbs, got {results:?}"
        );
    }
    assert!(
        drive.prompts[0].contains(&true),
        "the first prompt must offer a win, got {:?}",
        drive.prompts[0]
    );
    assert!(
        drive.prompts[1].contains(&false),
        "the second prompt must offer a loss, got {:?}",
        drive.prompts[1]
    );
    assert_eq!(
        drive.kept,
        vec![true, false],
        "the kept results are the win then the loss"
    );
    assert_eq!(
        coin_flips_by(&drive.events, P0),
        vec![true, false],
        "one CoinFlipped per logical flip, reporting the kept result"
    );

    let state = runner.state();
    assert!(
        state.battlefield.contains(&firecat),
        "Crazed Firecat must be on the battlefield"
    );
    assert_eq!(
        plus_one_counters(&runner, firecat),
        1,
        "one +1/+1 counter per won flip (the resumed win was counted)"
    );
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty(), "the trigger finished resolving");
    assert!(state.active_coin_flip_frame().is_none());
}

// --- T4: one Thumb per controller (scope sibling) ---------------------------

/// Scope sibling, NOT revert evidence: it passes before and after the classifier
/// fix, because a flip by P0 only ever had ONE candidate (P0's Thumb). It guards
/// against the classifier pulling an opponent's Thumb into the candidate set.
#[test]
fn thumbs_under_different_controllers_double_only_the_flippers_flip() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_thumb_for(&mut scenario, P0);
    add_thumb_for(&mut scenario, P1);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Puppet's Verdict", true, PUPPETS_VERDICT)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().rng = GameRng::seed_from_u64(0);

    cast_spell(&mut runner, spell, None);
    let (results, _) = pause_at_keep(&mut runner);
    assert_eq!(
        results.len(),
        2,
        "\"If YOU would flip\" scopes the replacement to the flipper: only P0's Thumb applies"
    );

    let keep = results.iter().position(|won| *won).unwrap_or(0);
    let keep_won = results[keep];
    let drive = drive_coin_flips(&mut runner, |_, _| keep);
    assert_eq!(drive.prompts.len(), 1);
    assert_eq!(
        coin_flips_by(&drive.events, P0),
        vec![keep_won],
        "the kept result is the one that happens"
    );
    let state = runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.stack.is_empty());
    assert!(state.active_coin_flip_frame().is_none());
}

// --- T5: the classifier's pipeline verdict ----------------------------------

/// `replace_event(CoinFlip { count: 1 })` on a fresh board carrying `thumbs`
/// Thumbs and a scaffold with `synthetics` definitions. Returns the applied count
/// and whether a CR 616.1 choice was parked.
fn flip_count(thumbs: usize, synthetics: Vec<ReplacementDefinition>) -> (u32, bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_gallery(&mut scenario);
    for _ in 0..thumbs {
        add_thumb(&mut scenario);
    }
    if !synthetics.is_empty() {
        add_synthetic_source(&mut scenario, synthetics);
    }
    let mut runner = scenario.build();

    let mut events = Vec::new();
    let proposed = ProposedEvent::CoinFlip {
        player_id: P0,
        count: 1,
        applied: HashSet::new(),
    };
    let count = match replace_event(runner.state_mut(), proposed, &mut events) {
        ReplacementResult::Execute(ProposedEvent::CoinFlip { count, .. }) => count,
        other => panic!(
            "commuting coin-flip count multipliers must auto-apply (CR 616.1f), got {other:?}"
        ),
    };
    let parked = runner.state().pending_replacement.is_some();
    (count, parked)
}

#[test]
fn three_thumbs_compound_coin_flip_to_eight() {
    // Positive sibling: two Thumbs scale one flip to four with no prompt.
    let (two, parked_two) = flip_count(2, vec![]);
    assert_eq!(
        two, 4,
        "two ×2 multipliers scale one flip to 2 × 2 = 4 (CR 616.1f)"
    );
    assert!(
        !parked_two,
        "commuting multipliers must not park a degenerate CR 616.1 choice"
    );

    let (three, parked_three) = flip_count(3, vec![]);
    assert_eq!(three, 8, "three ×2 multipliers compound to 2 × 2 × 2 = 8");
    assert!(
        !parked_three,
        "no CR 616.1 choice is parked for three Thumbs"
    );
}

/// The classifier reads the count EXPRESSION, not the card: a synthetic ×3
/// replacement commutes with Krark's ×2 in either order (6).
#[test]
fn coin_flip_count_multipliers_are_factor_agnostic() {
    let (six, parked) = flip_count(1, vec![factor_coin_flip_replacement(3)]);
    assert_eq!(six, 6, "×2 then ×3 (either order) is 6");
    assert!(
        !parked,
        "×2 and ×3 commute, so no ordering choice is parked"
    );
}

// --- T6: the classifier is not a blanket "CoinFlip ⇒ commute" ---------------

/// An offset rescale is NOT a multiplier, so it does not commute with Krark's ×2
/// and the CR 616.1 ordering choice stays real. This is the helper's `None` arm
/// (the first production branch the fixture reaches) followed by the execute
/// walk's conservative `Unconditional` default.
#[test]
fn non_multiplicative_coin_flip_replacement_stays_order_material() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_thumb(&mut scenario);
    add_synthetic_source(&mut scenario, vec![offset_coin_flip_replacement()]);
    let mut runner = scenario.build();

    let mut events = Vec::new();
    let proposed = ProposedEvent::CoinFlip {
        player_id: P0,
        count: 1,
        applied: HashSet::new(),
    };
    match replace_event(runner.state_mut(), proposed, &mut events) {
        ReplacementResult::NeedsChoice(player) => assert_eq!(
            player, P0,
            "the affected player (the flipper) makes the CR 616.1 choice"
        ),
        other => panic!(
            "a non-multiplicative count rescale must stay order-material (CR 616.1), got {other:?}"
        ),
    }
    assert!(
        runner.state().pending_replacement.is_some(),
        "the CR 616.1 ordering choice is parked for delivery"
    );
}
