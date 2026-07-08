//! Regression for issue #4356 — Trouble in Pairs must parse its disjunctive
//! trigger and draw when an opponent draws their second card each turn.
//!
//! https://github.com/phase-rs/phase/issues/4356

use engine::game::effects::draw::resolve as resolve_draw;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::process_triggers;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const TROUBLE_IN_PAIRS: &str = "If an opponent would begin an extra turn, that player skips that turn instead.\n\
Whenever an opponent attacks you with two or more creatures, draws their second card each turn, or casts their second spell each turn, you draw a card.";

fn hand_len(runner: &GameRunner, player: engine::types::player::PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn draw_for(runner: &mut GameRunner, player: engine::types::player::PlayerId) {
    let ability = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        Vec::new(),
        ObjectId(0),
        player,
    );
    let mut events = Vec::new();
    resolve_draw(runner.state_mut(), &ability, &mut events).expect("draw resolves");
    process_triggers(runner.state_mut(), &events);
    while !runner.state().stack.is_empty() {
        runner.advance_until_stack_empty();
    }
}

#[test]
fn trouble_in_pairs_oracle_parses_three_triggers_without_unimplemented() {
    let parsed = parse_oracle_text(
        TROUBLE_IN_PAIRS,
        "Trouble in Pairs",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    assert_eq!(
        parsed.triggers.len(),
        3,
        "Trouble in Pairs must emit three disjunctive triggers, got {:?}",
        parsed.triggers
    );
    for trigger in &parsed.triggers {
        let execute = trigger
            .execute
            .as_ref()
            .expect("each trigger must have execute");
        assert!(
            !matches!(execute.effect.as_ref(), Effect::Unimplemented { .. }),
            "trigger must not be Unimplemented: {trigger:?}"
        );
    }
}

#[test]
fn trouble_in_pairs_draws_when_opponent_draws_second_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Trouble in Pairs", 0, 0)
        .as_enchantment()
        .from_oracle_text(TROUBLE_IN_PAIRS);
    for i in 0..4 {
        scenario.add_card_to_library_top(P0, &format!("Card {i}"));
        scenario.add_card_to_library_top(P1, &format!("Opp Card {i}"));
    }

    let mut runner = scenario.build();
    runner.state_mut().players[P1.0 as usize].cards_drawn_this_turn = 1;

    let hand_before = hand_len(&runner, P0);
    draw_for(&mut runner, P1);

    assert_eq!(
        hand_len(&runner, P0),
        hand_before + 1,
        "controller must draw when an opponent draws their second card of the turn"
    );
}
