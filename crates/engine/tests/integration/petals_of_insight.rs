//! Integration tests for Petals of Insight (Champions of Kamigawa).
//!
//! Oracle: "Look at the top three cards of your library. You may put those cards
//! on the bottom of your library in any order. If you do, return Petals of
//! Insight to its owner's hand. Otherwise, draw three cards."
//!
//! Rules verification:
//! - CR 701.20e: Looking at cards in a library does not move them out of the library.
//! - CR 608.2c: An optional sub-ability ("You may...") pauses for the controller's decision.
//!   If accepted, "If you do" branches execute. If declined, "Otherwise" branches execute.
//! - CR 608.2m: As the final step of resolving an instant or sorcery, if it was not moved
//!   by an effect (e.g. self-bounce), it is placed into its owner's graveyard.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{DigRestOrder, DigSource, Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PETALS_OF_INSIGHT_ORACLE: &str = "Look at the top three cards of your library. \
You may put those cards on the bottom of your library in any order. \
If you do, return Petals of Insight to its owner's hand. Otherwise, draw three cards.";

fn petals_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue],
        generic: 4,
    }
}

fn setup_scenario() -> (GameRunner, ObjectId, [ObjectId; 3], [ObjectId; 3]) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Petals of Insight", false, PETALS_OF_INSIGHT_ORACLE)
        .with_mana_cost(petals_cost())
        .id();

    // 5 Islands to pay {4}{U}.
    for _ in 0..5 {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }

    // Add cards bottom-to-top so library[0] is Top A:
    let deep_c = scenario.add_card_to_library_top(P0, "Deep C");
    let deep_b = scenario.add_card_to_library_top(P0, "Deep B");
    let deep_a = scenario.add_card_to_library_top(P0, "Deep A");
    let top_c = scenario.add_card_to_library_top(P0, "Top C");
    let top_b = scenario.add_card_to_library_top(P0, "Top B");
    let top_a = scenario.add_card_to_library_top(P0, "Top A");

    let runner = scenario.build();
    (
        runner,
        spell,
        [top_a, top_b, top_c],
        [deep_a, deep_b, deep_c],
    )
}

#[test]
fn petals_of_insight_ast_shape() {
    let parsed = parse_oracle_text(
        PETALS_OF_INSIGHT_ORACLE,
        "Petals of Insight",
        &[],
        &["Sorcery".to_string(), "Arcane".to_string()],
        &[],
    );
    assert_eq!(parsed.abilities.len(), 1, "must have 1 root spell ability");
    let root = &parsed.abilities[0];

    // Root is look at top 3 cards (keep_count=0).
    match root.effect.as_ref() {
        Effect::Dig {
            count,
            keep_count,
            source,
            ..
        } => {
            assert_eq!(*count, QuantityExpr::Fixed { value: 3 });
            assert_eq!(*keep_count, Some(0));
            assert_eq!(*source, DigSource::Library);
        }
        other => panic!("expected Root Dig, got {:?}", other),
    }

    // Sub-ability is optional: put those cards on bottom of library in any order.
    let sub = root
        .sub_ability
        .as_ref()
        .expect("root must have sub_ability");
    assert!(sub.optional, "sub_ability must be optional ('You may')");
    match sub.effect.as_ref() {
        Effect::Dig {
            source,
            keep_count,
            rest_destination,
            rest_order,
            ..
        } => {
            assert_eq!(*source, DigSource::PriorLook);
            assert_eq!(*keep_count, Some(0));
            assert_eq!(*rest_destination, Some(Zone::Library));
            assert_eq!(*rest_order, DigRestOrder::Preserve);
        }
        other => panic!("expected PriorLook Dig, got {:?}", other),
    }

    // "If you do" branch: return Petals of Insight to owner's hand.
    let if_you_do = sub
        .sub_ability
        .as_ref()
        .expect("sub_ability must have 'If you do' sub_ability");
    assert!(
        if_you_do
            .condition
            .as_ref()
            .is_some_and(|c| c.is_optional_effect_performed()),
        "condition must be OptionalEffectPerformed"
    );
    match if_you_do.effect.as_ref() {
        Effect::Bounce { target, .. } => {
            assert_eq!(*target, TargetFilter::SelfRef);
        }
        other => panic!("expected Bounce SelfRef, got {:?}", other),
    }

    // "Otherwise" branch: draw three cards. Attached as else_ability of the if-you-do gate.
    let otherwise = if_you_do
        .else_ability
        .as_ref()
        .expect("if_you_do must have 'Otherwise' else_ability");
    match otherwise.effect.as_ref() {
        Effect::Draw { count, target } => {
            assert_eq!(*count, QuantityExpr::Fixed { value: 3 });
            assert_eq!(*target, TargetFilter::Controller);
        }
        other => panic!("expected Draw 3, got {:?}", other),
    }
}

#[test]
fn petals_of_insight_accept_puts_cards_on_bottom_and_bounces_to_hand() {
    let (mut runner, spell, [top_a, top_b, top_c], [deep_a, _, _]) = setup_scenario();

    // Cast Petals of Insight and stop at the optional decision prompt.
    let stopped = runner.cast(spell).commit().stop_at_optional().resolve();

    assert!(
        matches!(
            stopped.final_waiting_for(),
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "CR 608.2c: must pause for optional bottom placement choice, got {:?}",
        stopped.final_waiting_for()
    );

    // Private look IDs must hold the top 3 cards looked at.
    assert_eq!(
        runner.state().private_look_ids,
        vec![top_a, top_b, top_c],
        "CR 701.20e: private look IDs must contain the 3 cards"
    );

    // Player accepts the optional choice: put those cards on the bottom of library.
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept optional effect");

    // Stack must be empty and game waiting for priority.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "game must return to priority after resolution, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "stack must be empty");

    // Petals of Insight must be back in P0's hand, not in graveyard.
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Hand,
        "Petals of Insight must return to its owner's hand"
    );
    assert!(
        runner.state().players[P0.0 as usize].hand.contains(&spell),
        "P0's hand must contain Petals of Insight"
    );
    assert!(
        !runner.state().players[P0.0 as usize]
            .graveyard
            .contains(&spell),
        "P0's graveyard must NOT contain Petals of Insight"
    );

    // Library top must now be Deep A.
    let p0_library = &runner.state().players[P0.0 as usize].library;
    assert_eq!(
        p0_library[0], deep_a,
        "new library top must be Deep A after top 3 were put on bottom"
    );

    // Bottom of library must contain the 3 looked-at cards.
    let len = p0_library.len();
    assert_eq!(len, 6, "all 6 cards remain in library");
    let bottom_ids: Vec<ObjectId> = p0_library.iter().skip(len - 3).copied().collect();
    assert!(
        bottom_ids.contains(&top_a) && bottom_ids.contains(&top_b) && bottom_ids.contains(&top_c),
        "bottom of library must contain the 3 cards, got {:?}",
        bottom_ids
    );
}

#[test]
fn petals_of_insight_decline_draws_three_cards_and_goes_to_graveyard() {
    let (mut runner, spell, [top_a, top_b, top_c], [deep_a, _, _]) = setup_scenario();

    // Cast Petals of Insight and stop at the optional decision prompt.
    let stopped = runner.cast(spell).commit().stop_at_optional().resolve();

    assert!(
        matches!(
            stopped.final_waiting_for(),
            WaitingFor::OptionalEffectChoice { player: P0, .. }
        ),
        "CR 608.2c: must pause for optional bottom placement choice, got {:?}",
        stopped.final_waiting_for()
    );

    // Player declines the optional choice: "Otherwise, draw three cards."
    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline optional effect");

    // Stack must be empty and game waiting for priority.
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "game must return to priority after resolution, got {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty(), "stack must be empty");

    // P0 drew the top 3 cards into hand.
    let p0_hand = &runner.state().players[P0.0 as usize].hand;
    assert!(
        p0_hand.contains(&top_a) && p0_hand.contains(&top_b) && p0_hand.contains(&top_c),
        "P0 must have drawn Top A, Top B, and Top C into hand"
    );

    // Petals of Insight was NOT bounced, so it goes to graveyard (CR 608.2m).
    assert_eq!(
        runner.state().objects[&spell].zone,
        Zone::Graveyard,
        "CR 608.2m: Petals of Insight must go to graveyard upon resolution"
    );
    assert!(
        runner.state().players[P0.0 as usize]
            .graveyard
            .contains(&spell),
        "P0's graveyard must contain Petals of Insight"
    );

    // Library top must now be Deep A.
    let p0_library = &runner.state().players[P0.0 as usize].library;
    assert_eq!(
        p0_library[0], deep_a,
        "new library top must be Deep A after 3 cards drawn"
    );
    assert_eq!(p0_library.len(), 3, "3 cards remain in library");
}
