//! Issue #3872 — Tithe Taker's "During your turn" cost tax must apply only on
//! the static controller's turn, not the caster's turn.
//!
//! Oracle: "During your turn, spells your opponents cast cost {1} more to cast
//! and abilities your opponents activate cost {1} more to activate unless
//! they're mana abilities."
//!
//! Reported bug (#3872): the tax raised the *opponent's* spells on the
//! opponent's OWN turn, because the parser dropped the leading "During your
//! turn," timing restriction (the static parsed with `condition: None`).
//!
//! CR 102.1: the active player is the player whose turn it is. Two fixes
//! combine here: the cost-modifier parser now attaches
//! `StaticCondition::DuringYourTurn`, and that condition is evaluated against
//! the source permanent's controller (not the caster) so it is correct in the
//! cost-modification resolver, which passes the caster as the scope player.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle_static::parse_static_line;
use engine::types::ability::StaticCondition;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const TITHE_TAKER: &str = "During your turn, spells your opponents cast cost {1} more to cast and abilities your opponents activate cost {1} more to activate unless they're mana abilities.";

/// Begin casting P0's Lightning Bolt and return the total mana value of the
/// battlefield-modified cost the engine resolved for it. The cost is computed
/// when the spell is put on the stack (surfaced via `WaitingFor::TargetSelection`
/// before payment), so this reads the actual tax the cost resolver applied —
/// the {1} Tithe Taker increase is `mana_value() == 1`, no tax is `0`.
fn resolved_cost_mana_value(runner: &mut GameRunner, spell_id: ObjectId) -> u32 {
    let card_id = runner.state().objects[&spell_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the bolt should begin (cost is checked at payment, not here)");
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { pending_cast, .. } => pending_cast.cost.mana_value(),
        other => panic!("expected TargetSelection after casting the bolt, got {other:?}"),
    }
}

#[test]
fn tithe_taker_static_parses_with_during_your_turn_condition() {
    // CR 102.1: the leading "During your turn," timing restriction must lower
    // to a `StaticCondition::DuringYourTurn` gate on the cost-raise static —
    // not be silently dropped (which left `condition: None`, the root cause).
    let def = parse_static_line(TITHE_TAKER).expect("Tithe Taker static should parse");
    assert_eq!(
        def.condition,
        Some(StaticCondition::DuringYourTurn),
        "\"During your turn,\" must gate the cost-raise static, got {:?}",
        def.condition,
    );
}

#[test]
fn tithe_taker_does_not_tax_opponent_on_their_own_turn() {
    // P1 controls Tithe Taker. During P0's OWN turn, P0's Lightning Bolt must
    // NOT be taxed — the resolved cost carries no {1} increase. Before the fix
    // the dropped "During your turn" gate taxed it on every turn.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain); // active player = P0

    scenario
        .add_creature(P1, "Tithe Taker", 1, 2)
        .with_static_definition(
            parse_static_line(TITHE_TAKER).expect("Tithe Taker static should parse"),
        );
    let spell_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();
    assert_eq!(
        resolved_cost_mana_value(&mut runner, spell_id),
        0,
        "Tithe Taker must NOT tax an opponent's spell on the opponent's own turn (CR 102.1)",
    );
}

#[test]
fn tithe_taker_taxes_opponent_during_controllers_turn() {
    // P1 controls Tithe Taker. During P1's turn, P0's Lightning Bolt is taxed by
    // {1} — the resolver applies the increase when it is the static controller's
    // turn, confirming the gate is enabled (not merely disabled everywhere).
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P1, "Tithe Taker", 1, 2)
        .with_static_definition(
            parse_static_line(TITHE_TAKER).expect("Tithe Taker static should parse"),
        );
    let spell_id = scenario.add_bolt_to_hand(P0);

    let mut runner = scenario.build();
    // Move to P1's turn and hand P0 priority to cast its instant.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    assert_eq!(
        resolved_cost_mana_value(&mut runner, spell_id),
        1,
        "During the Tithe Taker controller's turn, the opponent's spell must be taxed by {{1}}",
    );
}
