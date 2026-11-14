//! Issue #5946 — Bogwater Lumaret ETB life-gain trigger storm softlock.
//!
//! When many creature tokens enter under Bogwater Lumaret, each ETB puts an
//! identical `GainLife { Fixed, Controller }` trigger on the stack. Without
//! proven-inert batching the engine resolves them one priority pass at a time.

use engine::game::perf_counters;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{AutoPassRequest, CastPaymentMode, WaitingFor};
use engine::types::identifiers::CardId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BOGWATER_ORACLE: &str =
    "Whenever this creature or another creature you control enters, you gain 1 life.";

const TOKEN_FLOOD_ORACLE: &str = "Create five 1/1 green Insect creature tokens.";

const TOKEN_COUNT: i32 = 5;

fn token_flood_spell(scenario: &mut GameScenario) -> engine::types::identifiers::ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Token Flood", true, TOKEN_FLOOD_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id()
}

fn resolve_spell_then_authorized_trigger_session(runner: &mut GameRunner) {
    runner
        .act(GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        })
        .expect("the spell resolves through a committed stack session");
    assert!(
        !runner.state().stack.is_empty(),
        "the spell resolution must leave its ETB trigger run on the stack"
    );
    runner
        .act(GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilStackEmpty,
        })
        .expect("a fresh committed session authorizes batching the trigger run");
}

/// Production oracle path: Bogwater on the battlefield, a flood of ETB tokens,
/// and proven-inert batching must collapse the life-gain run without a softlock.
#[test]
fn bogwater_token_etb_life_gain_batches_without_softlock() {
    perf_counters::reset();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);

    scenario.add_creature_from_oracle(P0, "Bogwater Lumaret", 2, 2, BOGWATER_ORACLE);
    let spell = token_flood_spell(&mut scenario);

    let mut runner = scenario.build();
    let card_id = CardId(spell.0);
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast token flood");

    resolve_spell_then_authorized_trigger_session(&mut runner);

    assert!(
        runner.state().stack.is_empty(),
        "all ETB life-gain triggers must resolve"
    );
    assert_eq!(
        runner.life(P0),
        20 + TOKEN_COUNT,
        "each token ETB must grant exactly 1 life via Bogwater"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(
        perf_counters::snapshot().stack_batched_entries >= TOKEN_COUNT as u64,
        "fixed-controller GainLife batch must consume the ETB trigger run"
    );
}

/// Paired positive without an observer still batches when two distinct ETB
/// life-gain sources contribute to the same contiguous run.
#[test]
fn interleaved_identical_etb_gainers_batch_source_independent() {
    perf_counters::reset();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, 20);

    scenario.add_creature_from_oracle(P0, "Bogwater Lumaret A", 2, 2, BOGWATER_ORACLE);
    scenario.add_creature_from_oracle(P0, "Bogwater Lumaret B", 2, 2, BOGWATER_ORACLE);
    let spell = token_flood_spell(&mut scenario);

    let mut runner = scenario.build();
    let card_id = CardId(spell.0);
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast token flood");
    resolve_spell_then_authorized_trigger_session(&mut runner);

    assert_eq!(runner.life(P0), 20 + TOKEN_COUNT * 2);
    assert!(runner.state().stack.is_empty());
    assert_eq!(
        perf_counters::snapshot().stack_batched_entries,
        (TOKEN_COUNT * 2) as u64,
        "SourceIndependent key must collapse interleaved identical ETB gainers"
    );
    assert_eq!(
        runner
            .state()
            .battlefield
            .iter()
            .filter(|id| runner.state().objects[id].zone == Zone::Battlefield)
            .count(),
        2 + TOKEN_COUNT as usize,
        "two Bogwaters plus token flood must remain on the battlefield"
    );
}
