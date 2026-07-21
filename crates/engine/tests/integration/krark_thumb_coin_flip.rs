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
use engine::types::ability::{Effect, QuantityExpr, ReplacementPlayerScope};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::proposed_event::ProposedEvent;
use engine::types::replacements::ReplacementEvent;
use engine::types::resolution::ResolutionStateWire;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
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
    runner.state_mut().rng = ChaCha20Rng::seed_from_u64(seed);
    (runner, spell)
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
    let v2 = serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
        .expect("real coin-flip prompt serializes as v2");
    assert_eq!(v2["resolution_state_version"], 2);
    assert!(v2.get("pending_coin_flip").is_none());
    let restored: ResolutionStateWire =
        serde_json::from_value(v2).expect("v2 coin-flip prompt round-trips");
    *runner.state_mut() = restored.into_game_state();
    assert!(
        runner.state().active_coin_flip_frame().is_some(),
        "the v2 round-trip preserves the prompt-owning coin-flip frame"
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
    runner.state_mut().rng = ChaCha20Rng::seed_from_u64(7);

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
    runner.state_mut().rng = ChaCha20Rng::seed_from_u64(3);

    let mut result = cast_and_resolve(&mut runner, spell);

    let mut kept_losses = 0;
    let mut prompts = 0;
    for _ in 0..60 {
        match result.waiting_for.clone() {
            WaitingFor::CoinFlipKeepChoice { results, .. } => {
                prompts += 1;
                assert_eq!(results.len(), 2);
                // Keep the first flip; track whether it was a loss.
                if !results[0] {
                    kept_losses += 1;
                }
                result = runner
                    .act(GameAction::SelectCoinFlips {
                        keep_indices: vec![0],
                    })
                    .expect("SelectCoinFlips must succeed");
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
}
