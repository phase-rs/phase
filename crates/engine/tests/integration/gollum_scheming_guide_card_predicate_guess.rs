//! Runtime coverage for Gollum, Scheming Guide's attack trigger.
//!
//! Oracle:
//! "Whenever Gollum attacks, look at the top two cards of your library, put
//! them back in any order, then choose land or nonland. An opponent guesses
//! whether the top card of your library is the chosen kind. Reveal that card.
//! If they guessed right, remove Gollum from combat. Otherwise, you draw a
//! card and Gollum can't be blocked this turn."
//!
//! This drives the production trigger/choice/resolution path through combat:
//! attack trigger -> library ordering -> controller card-predicate choice ->
//! opponent card-predicate guess -> reveal branch.

use engine::game::combat::{can_block_pair, AttackTarget};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, ChoiceType, Effect, FilterProp, PlayerFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

const GOLLUM_ORACLE: &str = "Whenever Gollum attacks, look at the top two cards of your library, put them back in any order, then choose land or nonland. An opponent guesses whether the top card of your library is the chosen kind. Reveal that card. If they guessed right, remove Gollum from combat. Otherwise, you draw a card and Gollum can't be blocked this turn.";

#[test]
fn gollum_attack_trigger_parser_promotes_choices_to_card_predicates() {
    let parsed = parse_oracle_text(
        GOLLUM_ORACLE,
        "Gollum, Scheming Guide",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let execute = parsed.triggers[0]
        .execute
        .as_ref()
        .expect("Gollum attack trigger must have an execute body");
    let controller_choice = execute
        .sub_ability
        .as_ref()
        .expect("controller chooses land or nonland");
    assert!(matches!(
        controller_choice.effect.as_ref(),
        Effect::Choose {
            choice_type: ChoiceType::CardPredicate { .. },
            persist: false,
            ..
        }
    ));

    let choose_opponent = controller_choice
        .sub_ability
        .as_ref()
        .expect("controller chooses the opponent who guesses");
    assert_eq!(choose_opponent.player_scope, None);
    assert!(matches!(
        choose_opponent.effect.as_ref(),
        Effect::Choose {
            choice_type: ChoiceType::Opponent { restriction: None },
            persist: false,
            ..
        }
    ));

    let opponent_guess = choose_opponent
        .sub_ability
        .as_ref()
        .expect("opponent guesses the top-card kind");
    assert_eq!(
        opponent_guess.player_scope,
        Some(PlayerFilter::ChosenPlayer { index: 0 })
    );
    assert!(matches!(
        opponent_guess.effect.as_ref(),
        Effect::Choose {
            choice_type: ChoiceType::CardPredicateGuess { .. },
            persist: false,
            ..
        }
    ));

    let reveal = opponent_guess
        .sub_ability
        .as_ref()
        .expect("top card reveal follows the guess");
    assert!(matches!(
        reveal.effect.as_ref(),
        Effect::RevealTop {
            player: engine::types::ability::TargetFilter::Controller,
            count: 1
        }
    ));
    let guessed_right = reveal
        .sub_ability
        .as_ref()
        .expect("guessed-right branch follows reveal");
    assert!(matches!(
        guessed_right.condition,
        Some(AbilityCondition::RevealedHasCardType {
            additional_filter: Some(FilterProp::MatchesLastChosenCardPredicate),
            ..
        })
    ));
}

fn scenario_with_gollum_for_players(
    player_count: u8,
    top_card_kind: CoreType,
) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    let mut scenario = if player_count == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(player_count, 42)
    };
    scenario.at_phase(Phase::PreCombatMain);
    let gollum = scenario
        .add_creature_from_oracle(P0, "Gollum, Scheming Guide", 2, 2, GOLLUM_ORACLE)
        .id();
    let blocker = scenario.add_creature(P1, "Ambush Blocker", 2, 2).id();

    let second = scenario.add_card_to_library_top(P0, "Coppercoat Vanguard");
    let top = scenario.add_card_to_library_top(
        P0,
        match top_card_kind {
            CoreType::Land => "Forest",
            _ => "Rally the Ranks",
        },
    );
    for player in 1..player_count {
        let player = PlayerId(player);
        for _ in 0..5 {
            scenario.add_card_to_library_top(player, "Plains");
        }
    }

    let mut runner = scenario.build();
    mark_core_type(&mut runner, top, top_card_kind);
    mark_core_type(&mut runner, second, CoreType::Creature);
    (runner, gollum, blocker, top)
}

fn scenario_with_gollum(top_card_kind: CoreType) -> (GameRunner, ObjectId, ObjectId, ObjectId) {
    scenario_with_gollum_for_players(2, top_card_kind)
}

fn mark_core_type(runner: &mut GameRunner, card: ObjectId, core_type: CoreType) {
    let object = runner
        .state_mut()
        .objects
        .get_mut(&card)
        .expect("card exists");
    object.card_types.core_types = vec![core_type];
    object.base_card_types = object.card_types.clone();
}

fn attack_with_gollum(runner: &mut GameRunner, gollum: ObjectId) {
    pass_priority_round(runner);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(gollum, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("Gollum should be able to attack");
}

fn keep_preferred_card_on_top(cards: Vec<ObjectId>, preferred_top: ObjectId) -> Vec<ObjectId> {
    let mut ordered = Vec::with_capacity(cards.len());
    if cards.contains(&preferred_top) {
        ordered.push(preferred_top);
    }
    ordered.extend(cards.into_iter().filter(|card| *card != preferred_top));
    ordered
}

fn drive_to_named_choice(runner: &mut GameRunner, preferred_top: ObjectId) {
    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { .. } => return,
            WaitingFor::Priority { .. } => pass_priority_round(runner),
            WaitingFor::ScryChoice { cards, .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: keep_preferred_card_on_top(cards, preferred_top),
                    })
                    .expect("keep the expected top card on top");
            }
            WaitingFor::DigChoice { cards, .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: keep_preferred_card_on_top(cards, preferred_top),
                    })
                    .expect("keep the expected top card on top");
            }
            other => panic!("expected progress toward NamedChoice, got {other:?}"),
        }
    }
    panic!(
        "never reached NamedChoice; last state = {:?}",
        runner.state().waiting_for
    );
}

fn pass_priority_round(runner: &mut GameRunner) {
    let seats = runner.state().seat_order.len();
    for _ in 0..seats {
        let _ = runner.act(GameAction::PassPriority);
    }
}

fn choose_card_predicate(
    runner: &mut GameRunner,
    expected_player: u8,
    expected_guess: bool,
    choice: &str,
) -> Vec<GameEvent> {
    let WaitingFor::NamedChoice {
        player,
        choice_type,
        options,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected NamedChoice before choosing {choice}, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        player.0, expected_player,
        "choice prompt belongs to the wrong player"
    );
    if expected_guess {
        assert!(
            matches!(choice_type, ChoiceType::CardPredicateGuess { .. }),
            "expected a card-predicate guess, got {choice_type:?}"
        );
    } else {
        assert!(
            matches!(choice_type, ChoiceType::CardPredicate { .. }),
            "expected a card-predicate choice, got {choice_type:?}"
        );
    }
    assert!(
        options.iter().any(|option| option == choice),
        "choice {choice} was not offered in {options:?}"
    );
    runner
        .act(GameAction::ChooseOption {
            choice: choice.to_string(),
        })
        .expect("card predicate choice should resolve")
        .events
}

fn choose_opponent(
    runner: &mut GameRunner,
    expected_chooser: PlayerId,
    opponent: PlayerId,
) -> Vec<GameEvent> {
    let WaitingFor::NamedChoice {
        player,
        choice_type,
        options,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected NamedChoice before choosing opponent, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, expected_chooser, "wrong player choosing opponent");
    assert!(
        matches!(choice_type, ChoiceType::Opponent { restriction: None }),
        "expected opponent choice, got {choice_type:?}"
    );
    let choice = opponent.0.to_string();
    assert!(
        options.iter().any(|option| option == &choice),
        "opponent {choice} was not offered in {options:?}"
    );
    runner
        .act(GameAction::ChooseOption { choice })
        .expect("opponent choice should resolve")
        .events
}

fn is_attacking(runner: &GameRunner, attacker: ObjectId) -> bool {
    runner.state().combat.as_ref().is_some_and(|combat| {
        combat
            .attackers
            .iter()
            .any(|entry| entry.object_id == attacker)
    })
}

fn drive_combat_damage_with_no_blocks(runner: &mut GameRunner) -> Vec<GameEvent> {
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        pass_priority_round(runner);
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .expect("declaring no blockers should succeed");
    }
    runner.combat_damage().events().to_vec()
}

fn resolve_gollum_choices(
    runner: &mut GameRunner,
    preferred_top: ObjectId,
    controller_choice: &str,
    opponent_guess: &str,
) -> Vec<GameEvent> {
    drive_to_named_choice(runner, preferred_top);
    let _ = choose_card_predicate(runner, P0.0, false, controller_choice);
    drive_to_named_choice(runner, preferred_top);
    let _ = choose_opponent(runner, P0, P1);
    drive_to_named_choice(runner, preferred_top);
    choose_card_predicate(runner, P1.0, true, opponent_guess)
}

#[test]
fn gollum_attack_trigger_removes_him_from_combat_when_guess_is_right() {
    let (mut runner, gollum, _blocker, top) = scenario_with_gollum(CoreType::Land);
    attack_with_gollum(&mut runner, gollum);

    let hand_before = runner.state().players[0].hand.len();
    let defender_life_before = runner.state().players[P1.0 as usize].life;
    let events = resolve_gollum_choices(&mut runner, top, "Land", "Land");
    runner.advance_until_stack_empty();
    let combat_events = drive_combat_damage_with_no_blocks(&mut runner);

    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::CardPredicateGuessMade {
                player_id,
                source_id: Some(source_id),
                choice,
            } if *player_id == P1 && *source_id == gollum && choice == "Land"
        )),
        "opponent guess should produce a generic predicate debug event, got {events:?}"
    );
    assert!(
        !is_attacking(&runner, gollum),
        "a correct top-card predicate guess must remove Gollum from combat"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before,
        "the correct-guess branch must not draw a card"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        defender_life_before,
        "removed-from-combat Gollum must not deal combat damage"
    );
    assert!(
        !combat_events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Player(P1),
                is_combat: true,
                ..
            } if *source_id == gollum
        )),
        "removed-from-combat Gollum must not emit combat damage events"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&gollum)
            .expect("Gollum exists")
            .chosen_attributes
            .is_empty(),
        "transient predicate choices must not leave a rendered source label"
    );
}

#[test]
fn gollum_attack_trigger_draws_and_cannot_be_blocked_when_guess_is_wrong() {
    let (mut runner, gollum, blocker, top) = scenario_with_gollum(CoreType::Land);
    attack_with_gollum(&mut runner, gollum);

    let hand_before = runner.state().players[0].hand.len();
    let defender_life_before = runner.state().players[P1.0 as usize].life;
    let events = resolve_gollum_choices(&mut runner, top, "Land", "Nonland");
    runner.advance_until_stack_empty();

    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::CardPredicateGuessMade {
                player_id,
                source_id: Some(source_id),
                choice,
            } if *player_id == P1 && *source_id == gollum && choice == "Nonland"
        )),
        "opponent guess should produce a generic predicate debug event, got {events:?}"
    );
    assert!(
        is_attacking(&runner, gollum),
        "an incorrect top-card predicate guess must leave Gollum attacking"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 1,
        "the wrong-guess branch must draw exactly one card"
    );
    assert!(
        runner.state().players[0].hand.contains(&top),
        "the revealed top card should be the card drawn by the wrong-guess branch"
    );
    assert!(
        runner
            .state()
            .objects
            .get(&gollum)
            .expect("Gollum exists")
            .chosen_attributes
            .is_empty(),
        "transient predicate choices must not leave a rendered source label"
    );

    assert!(
        !can_block_pair(runner.state(), blocker, gollum),
        "Gollum should not be a legal block target after the wrong-guess branch"
    );

    let combat_events = drive_combat_damage_with_no_blocks(&mut runner);
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        defender_life_before - 2,
        "wrong-guess branch must leave Gollum attacking so he deals 2 combat damage"
    );
    assert!(
        combat_events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Player(P1),
                amount: 2,
                is_combat: true,
                ..
            } if *source_id == gollum
        )),
        "wrong-guess branch must emit Gollum's combat damage event, got {combat_events:?}"
    );
}

#[test]
fn gollum_three_player_guess_prompts_only_the_chosen_opponent() {
    let (mut runner, gollum, _blocker, top) = scenario_with_gollum_for_players(3, CoreType::Land);
    attack_with_gollum(&mut runner, gollum);

    drive_to_named_choice(&mut runner, top);
    let _ = choose_card_predicate(&mut runner, P0.0, false, "Land");

    drive_to_named_choice(&mut runner, top);
    let _ = choose_opponent(&mut runner, P0, P2);

    drive_to_named_choice(&mut runner, top);
    let events = choose_card_predicate(&mut runner, P2.0, true, "Nonland");
    runner.advance_until_stack_empty();

    let guesses: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CardPredicateGuessMade {
                player_id,
                source_id: Some(source_id),
                choice,
            } if *source_id == gollum => Some((*player_id, choice.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        guesses,
        vec![(P2, "Nonland")],
        "only the single chosen opponent should guess; P1 must not receive a fan-out prompt"
    );
    assert!(
        is_attacking(&runner, gollum),
        "the chosen opponent's wrong guess should drive the branch outcome"
    );
}
