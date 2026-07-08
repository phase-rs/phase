//! Issue #1350: Reaver Cleaver combat-damage trigger scaffolding must not
//! orphan when lethal damage eliminates the damaged player mid-ordering.
//!
//! When the defending player has multiple simultaneous "when you're dealt
//! combat damage" triggers, CR 603.3b requires ordering before they hit the
//! stack. If SBAs eliminate that player before they choose, the engine must
//! prune the ordering pass and clear stale state on `GameOver` — not leave
//! `pending_trigger_order` behind.

use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

use super::rules::run_combat;

const REAVER_CLEAVER_ORACLE: &str = "Equipped creature gets +1/+1 and has trample and \
\"Whenever this creature deals combat damage to a player or planeswalker, create that many \
Treasure tokens.\"\nEquip {3}";

const COMBAT_DAMAGE_DRAW: &str = "Whenever you're dealt combat damage, draw a card.";
const COMBAT_DAMAGE_GAIN: &str = "Whenever you're dealt combat damage, you gain 1 life.";

fn setup_lethal_reaver_combat() -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two defender triggers force CR 603.3b ordering for P1 when combat damage lands.
    scenario
        .add_creature(P1, "Damage Draw", 0, 1)
        .from_oracle_text(COMBAT_DAMAGE_DRAW);
    scenario
        .add_creature(P1, "Damage Gain", 0, 1)
        .from_oracle_text(COMBAT_DAMAGE_GAIN);

    let attacker = scenario.add_creature(P0, "Raider", 10, 10).id();
    let equipment = scenario
        .add_creature(P0, "The Reaver Cleaver", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(REAVER_CLEAVER_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().players[1].life = 5;
    attach_to(runner.state_mut(), equipment, attacker);
    evaluate_layers(runner.state_mut());

    run_combat(&mut runner, vec![attacker], vec![]);
    runner
}

#[test]
fn lethal_reaver_cleaver_combat_clears_trigger_scaffolding_on_game_over() {
    let runner = setup_lethal_reaver_combat();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::GameOver { winner: Some(P0) }
        ),
        "lethal combat damage should end the game for P0, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_trigger_order.is_none(),
        "pending trigger ordering must not survive player elimination (#1350)"
    );
    assert!(
        runner.state().deferred_triggers.is_empty(),
        "deferred triggers must not be orphaned on GameOver (#1350)"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
        "must not be stuck waiting to order triggers for an eliminated player"
    );
}
