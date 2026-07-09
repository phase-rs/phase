//! Runtime coverage for GitHub issue #1058: Pact of Negation's deferred
//! "At the beginning of your next upkeep, pay {3}{U}{U}. If you don't, you
//! lose the game." must actually end the game when the controller can't or
//! doesn't pay, and must NOT end the game when they can and do.
//!
//! Scoped to the delayed-trigger + pay-or-lose chain itself (`Effect::
//! CreateDelayedTrigger { effect: PayCost { sub_ability: LoseTheGame } }`),
//! parsed from the real verbatim Oracle text, driven through the real
//! `GameRunner` turn/upkeep machinery — not the unrelated "Counter target
//! spell" head effect, which is well-tested elsewhere and isn't in dispute
//! for this issue. CR 118.12 (resolution-time cost) + CR 118.3 (can't pay an
//! incomplete cost) + CR 608.2c (order of instructions) + CR 603.7a (delayed
//! triggered abilities).

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// The upkeep-and-lose portion of Pact of Negation's real Oracle text,
/// verbatim (this line dispatches through `try_parse_at_next_phase_delayed_trigger`,
/// the same function whose doc comment names the Pact cycle explicitly).
const PACT_UPKEEP_CLAUSE: &str =
    "At the beginning of your next upkeep, pay {3}{U}{U}. If you don't, you lose the game.";

/// Drive the engine through the REAL phase machinery until P0 is in its own
/// upkeep step, exactly as the live driver does: drain trigger ordering,
/// pass priority otherwise. Bounded to guard stalls.
fn advance_to_p0_upkeep(runner: &mut GameRunner) {
    for _ in 0..400 {
        if runner.state().phase == Phase::Upkeep && runner.state().active_player == P0 {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            other => panic!("unexpected WaitingFor while advancing to P0 upkeep: {other:?}"),
        }
    }
    panic!(
        "failed to reach P0's upkeep (stuck at phase {:?}, active {:?})",
        runner.state().phase,
        runner.state().active_player
    );
}

/// Parse the upkeep clause and install its delayed trigger directly on P0's
/// stack-resolution context, mirroring what `Counter target spell.`'s
/// sub-ability chain does when the real card resolves — skips the unrelated
/// Counter head effect, which isn't in dispute for this issue.
fn install_pact_delayed_trigger(runner: &mut GameRunner) {
    let parsed = parse_oracle_text(
        PACT_UPKEEP_CLAUSE,
        "Pact of Negation",
        &[],
        &["Instant".to_string()],
        &[],
    );
    let def = parsed
        .abilities
        .first()
        .expect("upkeep clause parses to one ability");
    assert!(
        matches!(def.effect.as_ref(), Effect::CreateDelayedTrigger { .. }),
        "expected CreateDelayedTrigger, got {:?}",
        def.effect
    );
    let source_id = runner.state().objects.keys().next().copied().unwrap_or({
        // No objects yet (fresh scenario) — create a nominal source so the
        // delayed trigger has somewhere to point back to.
        engine::game::zones::create_object(
            runner.state_mut(),
            engine::types::identifiers::CardId(9001),
            P0,
            "Pact of Negation".to_string(),
            Zone::Graveyard,
        )
    });
    let ability = build_resolved_from_def(def, source_id, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("installing the delayed trigger must not error");
}

#[test]
fn pact_of_negation_loses_the_game_when_upkeep_cost_goes_unpaid() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Stock both libraries so neither player decks out (CR 104.3c) while we
    // advance turns to reach P0's next upkeep.
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest", "Forest", "Forest"]);
    let mut runner = scenario.build();

    // No lands anywhere — P0 cannot pay {3}{U}{U} at the relevant upkeep.
    install_pact_delayed_trigger(&mut runner);
    advance_to_p0_upkeep(&mut runner);
    runner.advance_until_stack_empty();

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "P0 must lose the game when the deferred {{3}}{{U}}{{U}} cost goes unpaid, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn pact_of_negation_does_not_lose_the_game_when_upkeep_cost_is_paid() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Forest", "Forest", "Forest", "Forest", "Forest"]);
    // {3}{U}{U} = 5 total mana; five Islands cover both the generic and
    // colored portions via auto-tap at resolution.
    for _ in 0..5 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }
    let mut runner = scenario.build();

    install_pact_delayed_trigger(&mut runner);
    advance_to_p0_upkeep(&mut runner);
    runner.advance_until_stack_empty();

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "P0 must NOT lose the game when the deferred cost is paid, got {:?}",
        runner.state().waiting_for
    );
}
