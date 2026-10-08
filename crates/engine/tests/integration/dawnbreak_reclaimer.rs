//! Regression coverage for Dawnbreak Reclaimer's reciprocal graveyard choices.
//!
//! The inline setup deliberately uses the parsed Oracle instruction rather
//! than a card-data fixture: the test exercises the production parser,
//! resolution continuation, and `GameAction` round trips without coupling this
//! focused engine fixture to an external card export.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::engine::apply;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::AbilityKind;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ORACLE: &str = "Choose a creature card in an opponent's graveyard, then that player chooses a creature card in your graveyard. You may return those cards to the battlefield under their owners' control.";

#[test]
fn dawnbreak_reclaimer_binds_the_second_choice_to_the_first_cards_owner() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_creature_to_graveyard(P1, "Opponent Graveyard Creature", 2, 2)
        .id();
    let second = scenario
        .add_creature_to_graveyard(P0, "Controller Graveyard Creature", 3, 3)
        .id();
    let source = scenario.add_basic_land(P0, ManaColor::White);
    let mut runner = scenario.build();

    let definition = parse_effect_chain(ORACLE, AbilityKind::Spell);
    let ability = build_resolved_from_def(&definition, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Dawnbreak's first choice reaches the interactive resolver");

    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P0);
            assert_eq!(cards, &vec![first]);
        }
        other => panic!("expected the first graveyard choice, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards { cards: vec![first] })
        .expect("the controller selects the opponent-owned creature");

    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P1, "the first card's owner chooses next");
            assert_eq!(cards, &vec![second]);
        }
        other => panic!("expected the reciprocal graveyard choice, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![second],
        })
        .expect("the first card's owner selects from the controller's graveyard");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the controller may return both selected cards");
    for (card, owner) in [(first, P1), (second, P0)] {
        let object = &runner.state().objects[&card];
        assert_eq!(object.zone, Zone::Battlefield);
        assert_eq!(object.controller, owner);
    }
}

#[test]
fn dawnbreak_reclaimer_empty_second_choice_reaches_the_optional_return() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario
        .add_creature_to_graveyard(P1, "Opponent Graveyard Creature", 2, 2)
        .id();
    let source = scenario.add_basic_land(P0, ManaColor::White);
    let mut runner = scenario.build();

    let definition = parse_effect_chain(ORACLE, AbilityKind::Spell);
    let ability = build_resolved_from_def(&definition, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("Dawnbreak's first choice reaches the interactive resolver");

    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
            assert_eq!(*player, P0);
            assert_eq!(cards, &vec![first]);
        }
        other => panic!("expected the first graveyard choice, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards { cards: vec![first] })
        .expect("the controller selects the opponent-owned creature");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the empty second choice does not wedge Dawnbreak's optional return");

    let object = &runner.state().objects[&first];
    assert_eq!(object.zone, Zone::Battlefield);
    assert_eq!(object.controller, P1);
}

/// The published Dawnbreak Reclaimer ruling keeps the second choice alive when
/// no opponent graveyard supplies the first card: the controller chooses an
/// opponent, who may still choose from the controller's graveyard.
#[test]
fn dawnbreak_reclaimer_empty_first_choice_still_offers_the_second_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let second = scenario
        .add_creature_to_graveyard(P0, "Controller Graveyard Creature", 3, 3)
        .id();
    let source = scenario.add_basic_land(P0, ManaColor::White);
    let mut runner = scenario.build();

    let definition = parse_effect_chain(ORACLE, AbilityKind::Spell);
    let ability = build_resolved_from_def(&definition, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("the empty first choice continues to Dawnbreak's reciprocal choice");

    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
            assert_eq!(
                *player, P1,
                "the only opponent chooses despite the empty first graveyard pool"
            );
            assert_eq!(cards, &vec![second]);
        }
        other => panic!("expected Dawnbreak's second graveyard choice, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards {
            cards: vec![second],
        })
        .expect("the chosen opponent selects the controller-owned creature");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the controller may return the one card selected by the second choice");
    let object = &runner.state().objects[&second];
    assert_eq!(object.zone, Zone::Battlefield);
    assert_eq!(object.controller, P0);
}

#[test]
fn dawnbreak_reclaimer_empty_first_choice_lets_the_controller_pick_the_second_chooser() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let second = scenario
        .add_creature_to_graveyard(P0, "Controller Graveyard Creature", 3, 3)
        .id();
    let source = scenario.add_basic_land(P0, ManaColor::White);
    let mut runner = scenario.build();

    let definition = parse_effect_chain(ORACLE, AbilityKind::Spell);
    let ability = build_resolved_from_def(&definition, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("the empty first choice reaches Dawnbreak's opponent chooser");

    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneOpponentChooser {
            player, candidates, ..
        } => {
            assert_eq!(*player, P0, "the controller picks the second chooser");
            assert_eq!(candidates, &vec![P1, p2]);
        }
        other => panic!("expected Dawnbreak's opponent chooser, got {other:?}"),
    }
    assert!(
        apply(
            runner.state_mut(),
            P1,
            GameAction::ChooseZoneOpponentChooser { opponent: p2 },
        )
        .is_err(),
        "a noncontroller cannot choose Dawnbreak's second chooser"
    );
    runner
        .act(GameAction::ChooseZoneOpponentChooser { opponent: p2 })
        .expect("the controller chooses P2 to make the reciprocal choice");

    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
            assert_eq!(*player, p2, "only the selected opponent chooses the card");
            assert_eq!(cards, &vec![second]);
        }
        other => panic!("expected Dawnbreak's second graveyard choice, got {other:?}"),
    }
    assert!(
        apply(
            runner.state_mut(),
            P1,
            GameAction::SelectCards {
                cards: vec![second],
            },
        )
        .is_err(),
        "a nonchosen opponent cannot submit the reciprocal card choice"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![second],
        })
        .expect("P2 selects the controller-owned creature");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));

    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("the controller may return the card selected by P2");
    let object = &runner.state().objects[&second];
    assert_eq!(object.zone, Zone::Battlefield);
    assert_eq!(object.controller, P0);
}
