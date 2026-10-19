//! Krark's Thumb — "If you would flip a coin, instead flip two coins and ignore
//! one." (CR 705.1 + CR 614.1a)
//!
//! Validates the reusable coin-flip replacement seam:
//! `ProposedEvent::CoinFlip` -> CR 614 replacement pipeline -> RNG, mirroring
//! Draw/Scry/Mill. Per the card's 2019-01-25 ruling, each individual flip is
//! replaced separately (an N-coin effect = N independent flip-2-keep-1 choices).
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 614.1a: "instead" is a replacement effect.
//!   - CR 614.6: a replaced event never happens (the ignored flips don't occur).
//!   - CR 705.1: to flip a coin, the player calls heads or tails, then flips it.
//!   - CR 705.2: only the flipping player wins or loses the flip.
//!
//! These are apply()-driven runtime tests: the interactive keep choice is
//! exercised through the real `apply()` pipeline, not a direct `resolve()`.

use engine::game::replacement::{replace_event, ReplacementResult};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::ability::{Effect, EffectKind, QuantityExpr, ReplacementPlayerScope};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::ActionResult;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::GameRng;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
use engine::types::replacements::ReplacementEvent;
use engine::types::resolution::{ResolutionStateWire, RESOLUTION_STATE_WIRE_VERSION};
use std::collections::HashSet;

/// Krark's Thumb printed Oracle text — byte-identical to card-data.json.
const KRARK: &str = "If you would flip a coin, instead flip two coins and ignore one.";

// --- Parser ----------------------------------------------------------------

/// CR 614.1a + CR 705.1: Krark's text parses into a controller-scoped `CoinFlip`
/// replacement that doubles the flip count, with no `valid_card` filter (the
/// replacement is objectless — it watches the controller's flips).
#[test]
fn parses_krark_coin_flip_replacement() {
    let parsed = parse_oracle_text(KRARK, "Krark's Thumb", &[], &[], &[]);
    let repl = parsed
        .replacements
        .iter()
        .find(|r| matches!(r.event, ReplacementEvent::CoinFlip))
        .expect("Krark must parse a CoinFlip replacement");

    assert_eq!(
        repl.valid_player,
        Some(ReplacementPlayerScope::You),
        "Krark is controller-scoped (\"If YOU would flip\")"
    );
    assert!(
        repl.valid_card.is_none(),
        "Krark's replacement is objectless — no valid_card filter"
    );

    let execute = repl
        .execute
        .as_ref()
        .expect("the replacement must carry an execute Template");
    match execute.effect.as_ref() {
        Effect::FlipCoins {
            count: QuantityExpr::Multiply { factor, .. },
            ..
        } => assert_eq!(*factor, 2, "Krark doubles the flip count"),
        other => panic!("expected FlipCoins {{ Multiply {{ factor: 2 }} }}, got {other:?}"),
    }
}

// --- Replacement applier / scoping -----------------------------------------

/// Put Krark's Thumb on the given player's battlefield (parsed from Oracle so
/// the test exercises the live replacement shape). Krark is an artifact, but the
/// replacement's controller scope is card-type-independent, so a creature body
/// is a harmless test scaffold.
fn add_krark(scenario: &mut GameScenario, player: PlayerId) -> ObjectId {
    scenario
        .add_creature_from_oracle(player, "Krark's Thumb", 0, 1, KRARK)
        .id()
}

/// CR 614.1a: with Krark on P0's battlefield, P0's `CoinFlip { count: 1 }` event
/// is doubled to `count: 2` by the replacement applier.
#[test]
fn applier_doubles_controllers_flip() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_krark(&mut scenario, P0);
    let mut runner = scenario.build();

    let mut events = Vec::new();
    let proposed = ProposedEvent::CoinFlip {
        player_id: P0,
        count: 1,
        applied: HashSet::new(),
    };
    match replace_event(runner.state_mut(), proposed, &mut events) {
        ReplacementResult::Execute(ProposedEvent::CoinFlip { count, .. }) => {
            assert_eq!(count, 2, "Krark doubles P0's flip");
        }
        other => panic!("expected Execute(CoinFlip {{ count: 2 }}), got {other:?}"),
    }
    assert!(
        !runner.state().has_post_replacement_drain(),
        "Krark's inline count replacement must not leave a dead post-replacement drain"
    );
}

/// CR 614.1a: Krark is controller-scoped (B2). With Krark under P0, a flip by P1
/// is NOT doubled; a flip by P0 IS doubled.
#[test]
fn applier_scopes_to_controller_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_krark(&mut scenario, P0);
    let mut runner = scenario.build();

    // P1's flip — not doubled.
    let mut events = Vec::new();
    let p1_flip = ProposedEvent::CoinFlip {
        player_id: P1,
        count: 1,
        applied: HashSet::new(),
    };
    match replace_event(runner.state_mut(), p1_flip, &mut events) {
        ReplacementResult::Execute(ProposedEvent::CoinFlip { count, .. }) => {
            assert_eq!(count, 1, "P1's flip is not doubled by P0's Krark");
        }
        other => panic!("expected Execute(CoinFlip {{ count: 1 }}), got {other:?}"),
    }

    // P0's flip — doubled.
    let mut events = Vec::new();
    let p0_flip = ProposedEvent::CoinFlip {
        player_id: P0,
        count: 1,
        applied: HashSet::new(),
    };
    match replace_event(runner.state_mut(), p0_flip, &mut events) {
        ReplacementResult::Execute(ProposedEvent::CoinFlip { count, .. }) => {
            assert_eq!(count, 2, "P0's own flip is doubled");
        }
        other => panic!("expected Execute(CoinFlip {{ count: 2 }}), got {other:?}"),
    }
}

// --- Runtime suspend / resume (apply-driven) -------------------------------

/// Add a no-cost instant to P0's hand whose only effect is `effect`, returning
/// its object id. Cast it via `act` and resolve via PassPriority.
fn add_flip_spell(scenario: &mut GameScenario, effect: Effect) -> ObjectId {
    let mut builder = scenario.add_spell_to_hand(P0, "Flip Spell", true);
    builder.with_ability(effect);
    builder.id()
}

/// Cast the flip spell and pass priority until either the stack starts resolving
/// (reaching a CoinFlipKeepChoice) or it fully resolves. Returns the
/// `ActionResult` of the action that produced the current `waiting_for`.
fn cast_and_resolve(
    runner: &mut engine::game::scenario::GameRunner,
    spell_id: ObjectId,
) -> engine::types::game_state::ActionResult {
    let card_id = runner.state().objects[&spell_id].card_id;
    let mut result = runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts the flip spell");

    for _ in 0..20 {
        match &result.waiting_for {
            WaitingFor::CoinFlipKeepChoice { .. } => return result,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return result,
            WaitingFor::Priority { .. } => {
                result = runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve the flip spell");
            }
            other => panic!("unexpected waiting state resolving flip: {other:?}"),
        }
    }
    panic!("flip spell did not resolve within 20 steps");
}

fn setup_single_flip(seed: u64) -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_krark(&mut scenario, P0);
    let spell = add_flip_spell(
        &mut scenario,
        Effect::FlipCoin {
            win_effect: None,
            lose_effect: None,
            flipper: engine::types::ability::TargetFilter::Controller,
        },
    );
    let mut runner = scenario.build();
    runner.state_mut().rng = GameRng::seed_from_u64(seed);
    (runner, spell)
}

/// Counts the `EffectResolved` markers a coin-flip run reported for `source`.
fn coin_run_markers(events: &[GameEvent], source: ObjectId) -> usize {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::FlipCoins,
                    source_id,
                    ..
                } if *source_id == source
            )
        })
        .count()
}

fn count_coin_flipped(events: &[GameEvent]) -> usize {
    events
        .iter()
        .filter(|e| matches!(e, GameEvent::CoinFlipped { .. }))
        .count()
}

fn life_of(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

/// "Flip a coin until you lose a flip. For each flip you won, you gain 1 life."
/// A synthetic spell body: the win branch is `GainLife 1`, there is no lose branch.
fn until_loss_gain_life() -> Effect {
    use engine::types::ability::{AbilityDefinition, AbilityKind, TargetFilter};

    Effect::FlipCoinUntilLose {
        win_effect: Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        )),
    }
}

/// Like `cast_and_resolve`, but also returns every event of the casting action
/// and each priority pass, and stops at the first non-Priority prompt (or at
/// Priority with an empty stack).
fn cast_and_collect(
    runner: &mut engine::game::scenario::GameRunner,
    spell_id: ObjectId,
) -> (ActionResult, Vec<GameEvent>) {
    let card_id = runner.state().objects[&spell_id].card_id;
    let mut result = runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts the flip spell");
    let mut events = result.events.clone();

    for _ in 0..20 {
        match &result.waiting_for {
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                result = runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve the flip spell");
                events.extend(result.events.iter().cloned());
            }
            _ => return (result, events),
        }
    }
    panic!("flip spell did not resolve within 20 steps");
}

/// One answered keep prompt, with the markers its own action reported.
struct KeepStep {
    kept_won: bool,
    markers: usize,
}

/// Keep index 0 at every `CoinFlipKeepChoice` until the run leaves the prompts,
/// passing priority for any stack that still has to resolve.
fn keep_first_until_done(
    runner: &mut engine::game::scenario::GameRunner,
    source: ObjectId,
    mut result: ActionResult,
) -> Vec<KeepStep> {
    let mut steps = Vec::new();
    for _ in 0..60 {
        match result.waiting_for.clone() {
            WaitingFor::CoinFlipKeepChoice { results, .. } => {
                result = runner
                    .act(GameAction::SelectCoinFlips {
                        keep_indices: vec![0],
                    })
                    .expect("SelectCoinFlips must succeed");
                steps.push(KeepStep {
                    kept_won: results[0],
                    markers: coin_run_markers(&result.events, source),
                });
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return steps,
            WaitingFor::Priority { .. } => {
                result = runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting state: {other:?}"),
        }
    }
    panic!("keep prompts did not finish within 60 steps");
}

/// B1 (objectless replacement not skipped) + G-NEW-1 (zero CoinFlipped while
/// suspended): with Krark on the battlefield, P0's FlipCoin suspends on a
/// `CoinFlipKeepChoice` carrying two results, keep_count 1, and emits NO
/// `CoinFlipped` event yet (CR 614.6: the ignored flips never happen).
#[test]
fn single_flip_suspends_for_keep_choice_with_no_coin_flipped() {
    let (mut runner, spell) = setup_single_flip(0);
    let result = cast_and_resolve(&mut runner, spell);

    match &result.waiting_for {
        WaitingFor::CoinFlipKeepChoice {
            player,
            results,
            keep_count,
        } => {
            assert_eq!(*player, P0);
            assert_eq!(results.len(), 2, "Krark doubled the single flip");
            assert_eq!(*keep_count, 1, "keep exactly one (ignore one)");
        }
        other => panic!("expected CoinFlipKeepChoice, got {other:?}"),
    }

    // CR 614.6: no flip has "happened" yet — the suspend step emits no
    // CoinFlipped event (the ignored flips never occur).
    assert!(
        !result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::CoinFlipped { .. })),
        "no CoinFlipped may be emitted while suspended for the keep choice"
    );
}

/// G-NEW-1 + keep round-trip: after `SelectCoinFlips { [i] }`, EXACTLY ONE
/// `CoinFlipped` fires with `won == results[i]`, and the engine returns to a
/// normal Priority state with the flip spell resolved.
#[test]
fn keeping_a_flip_emits_exactly_one_coin_flipped_and_returns_to_priority() {
    let (mut runner, spell) = setup_single_flip(0);
    let result = cast_and_resolve(&mut runner, spell);

    let (player, results) = match result.waiting_for {
        WaitingFor::CoinFlipKeepChoice {
            player, results, ..
        } => (player, results),
        other => panic!("expected CoinFlipKeepChoice, got {other:?}"),
    };

    assert!(
        runner.state().active_coin_flip_frame().is_some(),
        "the typed coin-flip frame owns the real keep prompt"
    );
    let current =
        serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
            .expect("real coin-flip prompt serializes through the current wire");
    assert_eq!(
        current["resolution_state_version"],
        RESOLUTION_STATE_WIRE_VERSION
    );
    assert!(current.get("pending_coin_flip").is_none());
    let restored: ResolutionStateWire =
        serde_json::from_value(current).expect("current coin-flip prompt round-trips");
    *runner.state_mut() = restored.into_game_state();
    assert!(
        runner.state().active_coin_flip_frame().is_some(),
        "the current round-trip preserves the prompt-owning coin-flip frame"
    );

    // Keep the first flip. (`act` dispatches as the current acting player, P0.)
    let _ = player;
    let keep_index = 0usize;
    let expected_won = results[keep_index];
    let keep_result = runner
        .act(GameAction::SelectCoinFlips {
            keep_indices: vec![keep_index],
        })
        .expect("SelectCoinFlips must succeed");

    let coin_flips: Vec<bool> = keep_result
        .events
        .iter()
        .filter_map(|e| match e {
            GameEvent::CoinFlipped { won, .. } => Some(*won),
            _ => None,
        })
        .collect();
    assert_eq!(
        coin_flips,
        vec![expected_won],
        "exactly one CoinFlipped with won == kept result"
    );

    // The flip effect completed and we are back at Priority.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "engine returns to Priority after the keep choice; got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().active_coin_flip_frame().is_none(),
        "no coin-flip frame remains after completion"
    );
}

/// B3: `FlipCoins { count: 3 }` with Krark produces THREE sequential
/// `CoinFlipKeepChoice` prompts (each individual flip is replaced separately per
/// the 2019-01-25 ruling), and exactly three `CoinFlipped` events total.
#[test]
fn flip_coins_three_with_krark_prompts_three_times() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_krark(&mut scenario, P0);
    let spell = add_flip_spell(
        &mut scenario,
        Effect::FlipCoins {
            count: QuantityExpr::Fixed { value: 3 },
            win_effect: None,
            lose_effect: None,
            flipper: engine::types::ability::TargetFilter::Controller,
        },
    );
    let mut runner = scenario.build();
    runner.state_mut().rng = GameRng::seed_from_u64(7);

    let mut result = cast_and_resolve(&mut runner, spell);

    let mut prompts = 0;
    let mut total_coin_flips = 0;
    for _ in 0..30 {
        match result.waiting_for.clone() {
            WaitingFor::CoinFlipKeepChoice { results, .. } => {
                prompts += 1;
                assert_eq!(results.len(), 2, "each individual flip is doubled");
                result = runner
                    .act(GameAction::SelectCoinFlips {
                        keep_indices: vec![0],
                    })
                    .expect("SelectCoinFlips must succeed");
                total_coin_flips += result
                    .events
                    .iter()
                    .filter(|e| matches!(e, GameEvent::CoinFlipped { .. }))
                    .count();
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                result = runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting state: {other:?}"),
        }
    }

    assert_eq!(prompts, 3, "three independent flips => three keep prompts");
    assert_eq!(
        total_coin_flips, 3,
        "exactly one CoinFlipped per kept flip (three total)"
    );
}

/// until-lose + Krark: `FlipCoinUntilLose` with Krark prompts a keep choice per
/// flip and terminates once a kept loss occurs, emitting one CoinFlipped per
/// kept flip and returning to Priority.
#[test]
fn flip_until_lose_with_krark_resolves_via_per_flip_keep_choices() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, TargetFilter};

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_krark(&mut scenario, P0);
    let spell = add_flip_spell(
        &mut scenario,
        Effect::FlipCoinUntilLose {
            win_effect: Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )),
        },
    );
    let mut runner = scenario.build();
    runner.state_mut().rng = GameRng::seed_from_u64(3);
    let start_life = life_of(&runner, P0);

    let mut result = cast_and_resolve(&mut runner, spell);
    let mut coin_flipped = count_coin_flipped(&result.events);

    let mut kept_losses = 0;
    let mut kept_wins = 0;
    let mut prompts = 0;
    for _ in 0..60 {
        match result.waiting_for.clone() {
            WaitingFor::CoinFlipKeepChoice { results, .. } => {
                prompts += 1;
                assert_eq!(results.len(), 2);
                // Keep the first flip; track whether it was a loss.
                if results[0] {
                    kept_wins += 1;
                } else {
                    kept_losses += 1;
                }
                result = runner
                    .act(GameAction::SelectCoinFlips {
                        keep_indices: vec![0],
                    })
                    .expect("SelectCoinFlips must succeed");
                coin_flipped += count_coin_flipped(&result.events);
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                result = runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting state: {other:?}"),
        }
    }

    assert!(prompts >= 1, "until-lose must prompt at least once");
    assert_eq!(
        kept_losses, 1,
        "the loop ends exactly when a kept flip is a loss"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "engine returns to Priority after until-lose completes; got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().active_coin_flip_frame().is_none());
    // CR 705.2 + CR 608.2c: each kept win pays its reward once the run ends.
    assert_eq!(
        life_of(&runner, P0) - start_life,
        kept_wins,
        "P0 gains 1 life per kept win"
    );
    // CR 614.6: only the kept flips happen, one per keep prompt.
    assert_eq!(
        coin_flipped, prompts,
        "exactly one CoinFlipped per keep prompt"
    );
}

// --- Run-level characterization --------------------------------------------

/// Build a P0 scenario holding `effect` as a no-cost instant, optionally with a
/// Krark's Thumb under `krark_owner`, and seed the RNG.
fn flip_run_runner(
    effect: Effect,
    krark_owner: Option<PlayerId>,
    seed: u64,
) -> (engine::game::scenario::GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if let Some(owner) = krark_owner {
        add_krark(&mut scenario, owner);
    }
    let spell = add_flip_spell(&mut scenario, effect);
    let mut runner = scenario.build();
    runner.state_mut().rng = GameRng::seed_from_u64(seed);
    (runner, spell)
}

/// CR 705.1 + CR 608.2c + CR 614.1a: "Flip a coin until you lose a flip. For each
/// flip you won, [reward]" under Krark's Thumb. Every flip is a separate keep
/// prompt, and the rewards are paid only once the run has ended on a kept loss,
/// so a kept win pays nothing yet.
#[test]
fn until_loss_rewards_wait_for_the_kept_loss() {
    let mut reached = false;
    for seed in 0..64u64 {
        let (mut runner, spell) = flip_run_runner(until_loss_gain_life(), Some(P0), seed);
        let start_life = life_of(&runner, P0);
        let (first, mut events) = cast_and_collect(&mut runner, spell);
        let WaitingFor::CoinFlipKeepChoice { results, .. } = first.waiting_for.clone() else {
            panic!(
                "expected the first keep prompt, got {:?}",
                first.waiting_for
            );
        };
        // Reach guard: keep a win at the first prompt, which needs a seed whose
        // first pair of flips holds one.
        let Some(win_index) = results.iter().position(|won| *won) else {
            continue;
        };
        reached = true;

        let mut result = runner
            .act(GameAction::SelectCoinFlips {
                keep_indices: vec![win_index],
            })
            .expect("SelectCoinFlips must succeed");
        events.extend(result.events.iter().cloned());
        assert!(
            matches!(result.waiting_for, WaitingFor::CoinFlipKeepChoice { .. }),
            "a kept win continues the run with the next flip's keep prompt, got {:?}",
            result.waiting_for
        );
        assert_eq!(
            life_of(&runner, P0),
            start_life,
            "a kept win pays nothing until the run ends"
        );

        // Finish the run: keep a loss whenever the pair holds one, else a win.
        let mut kept_wins = 1;
        let mut prompts = 1;
        for _ in 0..60 {
            match result.waiting_for.clone() {
                WaitingFor::CoinFlipKeepChoice { results, .. } => {
                    prompts += 1;
                    let (index, won) = match results.iter().position(|won| !*won) {
                        Some(index) => (index, false),
                        None => (0, true),
                    };
                    result = runner
                        .act(GameAction::SelectCoinFlips {
                            keep_indices: vec![index],
                        })
                        .expect("SelectCoinFlips must succeed");
                    events.extend(result.events.iter().cloned());
                    if won {
                        kept_wins += 1;
                        assert_eq!(
                            life_of(&runner, P0),
                            start_life,
                            "no reward is paid before the kept loss"
                        );
                    }
                }
                WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
                WaitingFor::Priority { .. } => {
                    result = runner.act(GameAction::PassPriority).expect("pass priority");
                    events.extend(result.events.iter().cloned());
                }
                other => panic!("unexpected waiting state: {other:?}"),
            }
        }

        assert_eq!(
            life_of(&runner, P0) - start_life,
            kept_wins,
            "the kept loss pays exactly one reward per kept win"
        );
        assert_eq!(
            count_coin_flipped(&events),
            prompts,
            "one CoinFlipped per keep prompt"
        );
        assert_eq!(
            coin_run_markers(&events, spell),
            1,
            "the run reports resolved once, at the kept loss"
        );
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().active_coin_flip_frame().is_none());
        break;
    }
    assert!(reached, "some seed in 0..64 must open with a winning flip");
}

/// CR 614.1a: Krark's Thumb replaces only its controller's flips. With the Thumb
/// under P1, P0's until-loss run is never doubled: no keep prompt, every flip is
/// P0's, and the run reports resolved once.
#[test]
fn opponents_thumb_does_not_double_an_until_loss_run() {
    let mut longest_run = 0;
    for seed in 0..16u64 {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let thumb = add_krark(&mut scenario, P1);
        let spell = add_flip_spell(&mut scenario, until_loss_gain_life());
        let mut runner = scenario.build();
        runner.state_mut().rng = GameRng::seed_from_u64(seed);
        assert_eq!(
            runner.state().objects[&thumb].controller,
            P1,
            "the Thumb is the opponent's"
        );
        let start_life = life_of(&runner, P0);

        let (result, events) = cast_and_collect(&mut runner, spell);
        assert!(
            matches!(result.waiting_for, WaitingFor::Priority { .. })
                && runner.state().stack.is_empty(),
            "an undoubled run resolves without a keep prompt, got {:?}",
            result.waiting_for
        );
        let flips: Vec<(PlayerId, bool)> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::CoinFlipped { player_id, won } => Some((*player_id, *won)),
                _ => None,
            })
            .collect();
        assert!(!flips.is_empty(), "the run flipped (seed {seed})");
        assert!(
            flips.iter().all(|(player, _)| *player == P0),
            "every flip is P0's (seed {seed}): {flips:?}"
        );
        assert_eq!(
            flips.last().map(|(_, won)| *won),
            Some(false),
            "the run ends on a loss (seed {seed}): {flips:?}"
        );
        let wins = flips.iter().filter(|(_, won)| *won).count() as i32;
        assert_eq!(life_of(&runner, P0) - start_life, wins);
        assert_eq!(coin_run_markers(&events, spell), 1, "seed {seed}");
        longest_run = longest_run.max(flips.len());
    }
    // Reach guard: some run had at least one win before the loss, so the run
    // really did loop with the opponent's Thumb on the battlefield.
    assert!(longest_run >= 2, "some seed in 0..16 must win a flip first");
}

/// CR 608.2c: "flip a coin until you lose a flip. For each flip you won, [reward]"
/// where the reward asks a choice. All flips come first; answering the choice
/// resumes the rest of the spell. Exactly one won flip, so exactly one reward.
#[test]
fn suspended_until_loss_reward_drains_the_outer_continuation() {
    use engine::types::ability::{AbilityDefinition, AbilityKind, TargetFilter};

    let mut reached = false;
    for seed in 0..64u64 {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        for i in 0..5 {
            scenario.add_card_to_library_top(P0, &format!("Library Card {i}"));
        }
        let mut reward = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            },
        );
        reward.optional = true;
        let mut definition = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::FlipCoinUntilLose {
                win_effect: Box::new(reward),
            },
        );
        definition.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )));
        let mut builder = scenario.add_spell_to_hand(P0, "Flip Spell", true);
        builder.with_ability_definition(definition);
        let spell = builder.id();
        let mut runner = scenario.build();
        runner.state_mut().rng = GameRng::seed_from_u64(seed);
        let start_life = life_of(&runner, P0);

        let (prompt, events) = cast_and_collect(&mut runner, spell);
        let flips: Vec<bool> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::CoinFlipped { won, .. } => Some(*won),
                _ => None,
            })
            .collect();
        // Reach guard: the run is exactly one won flip, then the loss.
        if flips != [true, false] {
            continue;
        }
        reached = true;

        assert!(
            matches!(prompt.waiting_for, WaitingFor::OptionalEffectChoice { .. }),
            "the one reward asks its optional choice, got {:?}",
            prompt.waiting_for
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeChanged { .. })),
            "no reward is paid before the choice is answered"
        );
        assert_eq!(
            coin_run_markers(&events, spell),
            0,
            "a run whose reward is still suspended reports nothing yet"
        );
        let hand_before = runner.state().players[0].hand.len();

        let accepted = runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("DecideOptionalEffect must succeed");
        assert_eq!(
            life_of(&runner, P0) - start_life,
            1,
            "the accepted reward gained 1 life"
        );
        assert_eq!(
            runner.state().players[0].hand.len(),
            hand_before + 1,
            "the spell's remaining instruction (draw a card) ran after the choice"
        );
        assert!(
            !matches!(
                accepted.waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ),
            "exactly one optional choice is raised for a single won flip"
        );
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().stack.is_empty(), "the spell left the stack");
        assert!(runner.state().active_coin_flip_frame().is_none());
        break;
    }
    assert!(
        reached,
        "some seed in 0..64 must win exactly one flip, then lose"
    );
}

/// Engine contract: a run completing with no pending resolution choice reports
/// `EffectResolved` once, on every kept completion path under Krark's Thumb.
#[test]
fn every_kept_completion_reports_resolved_once() {
    // (a) A single kept flip.
    let (mut runner, spell) = setup_single_flip(0);
    let prompt = cast_and_resolve(&mut runner, spell);
    assert_eq!(
        coin_run_markers(&prompt.events, spell),
        0,
        "no marker while the keep prompt is open"
    );
    let steps = keep_first_until_done(&mut runner, spell, prompt);
    assert_eq!(steps.len(), 1, "a single flip has one keep prompt");
    assert_eq!(steps[0].markers, 1, "the kept single flip reports once");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    // (b) Three flips: only the final kept flip reports.
    let (mut runner, spell) = flip_run_runner(
        Effect::FlipCoins {
            count: QuantityExpr::Fixed { value: 3 },
            win_effect: None,
            lose_effect: None,
            flipper: engine::types::ability::TargetFilter::Controller,
        },
        Some(P0),
        7,
    );
    let prompt = cast_and_resolve(&mut runner, spell);
    let steps = keep_first_until_done(&mut runner, spell, prompt);
    assert_eq!(
        steps.iter().map(|step| step.markers).collect::<Vec<_>>(),
        vec![0, 0, 1],
        "three prompts; the marker comes with the third kept flip"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));

    // (c) Until-loss: every kept win reports nothing, the kept loss reports once.
    let (mut runner, spell) = flip_run_runner(until_loss_gain_life(), Some(P0), 3);
    let prompt = cast_and_resolve(&mut runner, spell);
    let steps = keep_first_until_done(&mut runner, spell, prompt);
    assert!(!steps.is_empty(), "until-loss prompts at least once");
    let last = steps.len() - 1;
    for (index, step) in steps.iter().enumerate() {
        assert_eq!(
            step.kept_won,
            index != last,
            "only the final kept flip is the loss (step {index})"
        );
        assert_eq!(
            step.markers,
            usize::from(index == last),
            "marker count at step {index}"
        );
    }
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
