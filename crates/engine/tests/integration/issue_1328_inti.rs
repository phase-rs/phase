//! Regression for issue #1328 — Inti, Seneschal of the Sun.
//!
//! Oracle (first ability):
//!   Whenever you attack, you may discard a card. When you do, put a +1/+1
//!   counter on target attacking creature. It gains trample until end of turn.
//!
//! CR 603.12: the +1/+1 counter is a reflexive triggered ability that targets
//! an attacking creature when the optional discard is performed.
//!
//! https://github.com/phase-rs/phase/issues/1328

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

use super::rules::AttackTarget;

const INTI_ATTACK_ABILITY: &str = "Whenever you attack, you may discard a card. When you do, put a +1/+1 counter on target attacking creature. It gains trample until end of turn.";

#[test]
fn inti_attack_trigger_ast_has_reflexive_counter_after_optional_discard() {
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::{AbilityCondition, Effect};

    let parsed = parse_oracle_text(
        INTI_ATTACK_ABILITY,
        "Inti, Seneschal of the Sun",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Knight".to_string()],
    );
    let execute = parsed
        .triggers
        .first()
        .and_then(|t| t.execute.as_ref())
        .expect("Inti must parse an attack trigger");

    assert!(
        matches!(*execute.effect, Effect::Discard { .. }),
        "root effect must be optional discard, got {:?}",
        execute.effect
    );
    assert!(execute.optional, "discard must be optional");
    let sub = execute
        .sub_ability
        .as_ref()
        .expect("discard must chain to reflexive sub-ability");
    assert_eq!(
        sub.condition,
        Some(AbilityCondition::WhenYouDo),
        "reflexive sub must be gated by WhenYouDo"
    );
    assert!(
        matches!(*sub.effect, Effect::PutCounter { .. }),
        "reflexive sub root must put a counter, got {:?}",
        sub.effect
    );
}

fn p1p1(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

#[test]
fn inti_reflexive_counter_prompts_for_attacking_creature_after_discard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Inti, Seneschal of the Sun", 2, 2, INTI_ATTACK_ABILITY);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let _hand_card = scenario.add_card_to_hand(P0, "Hand Card");

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare attackers");
    runner.pass_both_players();

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept optional discard");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "reflexive ability must prompt for an attacking creature target after discard, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn inti_reflexive_counter_puts_plus_one_on_chosen_attacker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Inti, Seneschal of the Sun", 2, 2, INTI_ATTACK_ABILITY);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let _hand_card = scenario.add_card_to_hand(P0, "Hand Card");

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare attackers");
    runner.pass_both_players();

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept optional discard");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(attacker)),
        })
        .expect("choose attacking creature for reflexive counter");
    runner.advance_until_stack_empty();

    assert_eq!(
        p1p1(&runner, attacker),
        1,
        "Inti's reflexive ability must put a +1/+1 counter on the chosen attacker"
    );
}

#[test]
fn inti_reflexive_counter_after_interactive_discard_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Inti, Seneschal of the Sun", 2, 2, INTI_ATTACK_ABILITY);
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let discard_a = scenario.add_card_to_hand(P0, "Discard A");
    let _discard_b = scenario.add_card_to_hand(P0, "Discard B");

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare attackers");
    runner.pass_both_players();

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept optional discard");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::DiscardChoice { .. }),
        "multiple cards in hand must prompt DiscardChoice before reflexive targeting, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![discard_a],
        })
        .expect("choose card to discard");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "reflexive ability must prompt for attacking creature after discard resolves, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(attacker)),
        })
        .expect("choose attacking creature");
    runner.advance_until_stack_empty();

    assert_eq!(
        p1p1(&runner, attacker),
        1,
        "Inti must put a +1/+1 counter on the attacker after interactive discard"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&attacker)
            .expect("attacker should remain on battlefield")
            .has_keyword(&Keyword::Trample),
        "Inti must also grant trample to the chosen attacker"
    );
}
