//! CR 608.2c + CR 119.4 + CR 118.12 (issue #11294): Niv-Mizzet, Ghost
//! Counsel — "Whenever you gain life, you may pay that much life. If you do,
//! draw that many cards."
//!
//! The payment amount is the triggering life-gain amount, read at resolution
//! through the shared `EventContextAmount` cascade. These tests drive the
//! production trigger pipeline: the card's own `{T}` engine (gain 1), an
//! outside gain of 6 (Nourish, proving the amount binds rather than defaulting
//! to 1), the decline branch, and the can't-afford branch (interleaved damage
//! drops life below the triggered amount before the trigger resolves).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const NIV_MIZZET: &str = "Niv-Mizzet, Ghost Counsel";
const NIV_MIZZET_ORACLE: &str = "Flying\nWhenever you gain life, you may pay that much life. If you do, draw that many cards.\n{T}: Each opponent loses 1 life and you gain 1 life.";
const NOURISH_ORACLE: &str = "You gain 6 life.";
const LIGHTNING_BOLT_ORACLE: &str = "Lightning Bolt deals 3 damage to any target.";

fn mana_pool(player_mana: &[(ManaType, usize)]) -> Vec<ManaUnit> {
    player_mana
        .iter()
        .flat_map(|(ty, n)| (0..*n).map(|_| ManaUnit::new(*ty, ObjectId(0), false, vec![])))
        .collect()
}

fn life_of(runner: &GameRunner, player: engine::types::player::PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

fn hand_len_of(runner: &GameRunner, player: engine::types::player::PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

/// Stage `count` library cards so draws are observable (and a wrongful draw in
/// a no-draw test would show up instead of fizzling on an empty library).
fn stage_library(scenario: &mut GameScenario, count: usize) {
    for i in 0..count {
        scenario.add_card_to_library_top(P0, &format!("Library Card {i}"));
    }
}

fn add_niv_mizzet(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, NIV_MIZZET, 4, 4, NIV_MIZZET_ORACLE)
        .id()
}

/// CR 119.3 + CR 121.1: the `{T}` engine gains 1, the trigger offers the
/// payment, accepting pays 1 life and draws 1 card.
#[test]
fn niv_mizzet_tap_engine_accept_pays_1_draws_1() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let niv = add_niv_mizzet(&mut scenario);
    stage_library(&mut scenario, 2);
    let mut runner = scenario.build();

    let life_p0_before = life_of(&runner, P0);
    let life_p1_before = life_of(&runner, P1);
    let hand_before = hand_len_of(&runner, P0);

    let outcome = runner.activate(niv, 0).stop_at_optional().resolve();
    match outcome.final_waiting_for() {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(*player, P0, "the pay prompt goes to the controller")
        }
        other => panic!("expected the pay prompt, got {other:?}"),
    }

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept is legal");
    runner.advance_until_stack_empty();

    assert_eq!(
        life_of(&runner, P0) - life_p0_before,
        0,
        "P0 gains 1 then pays 1"
    );
    assert_eq!(
        life_of(&runner, P1) - life_p1_before,
        -1,
        "each opponent loses 1 life"
    );
    assert_eq!(
        hand_len_of(&runner, P0) - hand_before,
        1,
        "accepting draws that many (1) cards"
    );
    assert!(
        runner.state().stack.is_empty(),
        "stack empties after the trigger resolves"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "clean halt at priority, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 608.2c: the payment and the draw both read the triggering event's amount
/// (6), not a default 1.
#[test]
fn niv_mizzet_outside_gain_6_accept_pays_6_draws_6() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_niv_mizzet(&mut scenario);
    let nourish = scenario
        .add_spell_to_hand_from_oracle(P0, "Nourish", true, NOURISH_ORACLE)
        .id();
    scenario.with_mana_pool(P0, mana_pool(&[(ManaType::Green, 2)]));
    stage_library(&mut scenario, 7);
    let mut runner = scenario.build();

    let life_p0_before = life_of(&runner, P0);
    let hand_before = hand_len_of(&runner, P0);

    let outcome = runner.cast(nourish).commit().stop_at_optional().resolve();
    match outcome.final_waiting_for() {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(*player, P0, "the pay prompt goes to the controller")
        }
        other => panic!("expected the pay prompt, got {other:?}"),
    }

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept is legal");
    runner.advance_until_stack_empty();

    assert_eq!(
        life_of(&runner, P0) - life_p0_before,
        0,
        "P0 gains 6 then pays 6"
    );
    // Nourish leaves the hand when the cast commits (CR 601.2a), so the net
    // hand change is draws minus the cast spell.
    assert_eq!(
        hand_len_of(&runner, P0) - hand_before,
        5,
        "accepting draws that many (6) cards"
    );
    assert!(
        runner.state().stack.is_empty(),
        "stack empties after the trigger resolves"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "clean halt at priority, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 118.12: declining keeps the gained life and draws nothing.
#[test]
fn niv_mizzet_decline_keeps_life_draws_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let niv = add_niv_mizzet(&mut scenario);
    stage_library(&mut scenario, 2);
    let mut runner = scenario.build();

    let life_p0_before = life_of(&runner, P0);
    let life_p1_before = life_of(&runner, P1);
    let hand_before = hand_len_of(&runner, P0);

    let outcome = runner.activate(niv, 0).stop_at_optional().resolve();
    match outcome.final_waiting_for() {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(*player, P0, "the pay prompt goes to the controller")
        }
        other => panic!("expected the pay prompt, got {other:?}"),
    }

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline is legal");
    runner.advance_until_stack_empty();

    assert_eq!(
        life_of(&runner, P0) - life_p0_before,
        1,
        "declining keeps the gained life"
    );
    assert_eq!(
        life_of(&runner, P1) - life_p1_before,
        -1,
        "each opponent still loses 1 life"
    );
    assert_eq!(
        hand_len_of(&runner, P0),
        hand_before,
        "declining draws nothing"
    );
    assert!(
        runner.state().stack.is_empty(),
        "stack empties after the declined trigger"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "clean halt at priority, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 119.4: P0 at life 2 gains 6 (trigger amount 6), then takes 3 (Bolt)
/// in response to the trigger; at resolution life 5 < 6, so the payment is
/// impossible — the trigger auto-declines, drawing nothing and paying nothing.
#[test]
fn niv_mizzet_cant_afford_auto_declines_draws_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 2);
    add_niv_mizzet(&mut scenario);
    let nourish = scenario
        .add_spell_to_hand_from_oracle(P0, "Nourish", true, NOURISH_ORACLE)
        .id();
    let bolt = scenario
        .add_spell_to_hand_from_oracle(P1, "Lightning Bolt", true, LIGHTNING_BOLT_ORACLE)
        .id();
    scenario.with_mana_pool(P0, mana_pool(&[(ManaType::Green, 2)]));
    scenario.with_mana_pool(P1, mana_pool(&[(ManaType::Red, 1)]));
    stage_library(&mut scenario, 7);
    let mut runner = scenario.build();
    let hand_before = hand_len_of(&runner, P0);

    runner.cast(nourish).commit();
    let mut bolt_cast = false;
    let mut trigger_seen = false;
    let mut prompt_offered = false;
    for _ in 0..24 {
        if runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { player } => {
                // Reach-guard: the trigger queues behind its own stack entry
                // (spells keep their object id on the stack; the trigger does
                // not match either spell).
                trigger_seen |= runner
                    .state()
                    .stack
                    .iter()
                    .any(|e| e.id != nourish && e.id != bolt);
                let nourish_on_stack = runner.state().stack.iter().any(|e| e.id == nourish);
                if player == P1 && !nourish_on_stack && !bolt_cast {
                    // Stop at the optional decision: the outer loop below is
                    // the only code that may observe an OptionalEffectChoice,
                    // so a wrongly offered prompt trips prompt_offered instead
                    // of being silently declined by the driver's default.
                    runner
                        .cast(bolt)
                        .target_player(P0)
                        .commit()
                        .stop_at_optional()
                        .resolve();
                    bolt_cast = true;
                } else {
                    runner.act(GameAction::PassPriority).expect("pass");
                }
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                prompt_offered = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline is legal");
            }
            other => panic!("unexpected halt: {other:?}"),
        }
    }

    assert!(bolt_cast, "the interleaved Bolt resolved");
    assert!(
        trigger_seen,
        "the life-gain trigger queued (else no-draw is vacuous)"
    );
    assert!(
        !prompt_offered,
        "an unpayable optional payment auto-declines without prompting"
    );
    assert_eq!(life_of(&runner, P0), 5, "2 + 6 gained - 3 bolted, 0 paid");
    assert_eq!(
        hand_len_of(&runner, P0),
        hand_before - 1,
        "Nourish cast, nothing drawn"
    );
    assert!(
        runner.state().stack.is_empty(),
        "stack empties after the auto-declined trigger"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "clean halt at priority, got {:?}",
        runner.state().waiting_for
    );
}
