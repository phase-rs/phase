//! Regression for GitHub issue #4232 — Winota's selected Human must enter
//! tapped and attacking from the attack-trigger Dig.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const WINOTA_ORACLE: &str = "Whenever a non-Human creature you control attacks, look at the top six cards of your library. You may put a Human creature card from among them onto the battlefield tapped and attacking. It gains indestructible until end of turn. Put the rest of the cards on the bottom of your library in a random order.";

fn advance_to_dig_choice(runner: &mut GameRunner) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DigChoice { .. } => return,
            WaitingFor::OrderTriggers { triggers, .. } => runner
                .act(GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                })
                .expect("order Winota's trigger"),
            WaitingFor::Priority { .. } => runner
                .act(GameAction::PassPriority)
                .expect("pass priority toward Winota's DigChoice"),
            other => panic!("unexpected state before Winota's DigChoice: {other:?}"),
        };
    }
    panic!("Winota's attack trigger never reached DigChoice");
}

/// CR 508.4 + CR 506.3: the controller chooses a legal defender for a creature
/// that enters attacking, independent of the trigger's original attack target.
#[test]
fn winota_puts_selected_human_onto_battlefield_tapped_and_attacking() {
    let p2 = PlayerId(2);
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let human = scenario.add_card_to_library_top(P0, "Winota Human");
    for name in ["Filler 1", "Filler 2", "Filler 3", "Filler 4", "Filler 5"] {
        scenario.add_card_to_library_top(P0, name);
    }
    let _winota = scenario
        .add_creature_from_oracle(P0, "Winota, Joiner of Forces", 4, 4, WINOTA_ORACLE)
        .with_subtypes(vec!["Human", "Warrior"])
        .id();
    let attacker = scenario
        .add_creature(P0, "Non-Human Attacker", 2, 2)
        .with_subtypes(vec!["Goblin"])
        .id();

    let mut runner = scenario.build();
    {
        let human_card = runner.state_mut().objects.get_mut(&human).unwrap();
        human_card.card_types.core_types = vec![CoreType::Creature];
        human_card.card_types.subtypes = vec!["Human".to_string()];
        human_card.base_card_types = human_card.card_types.clone();
    }

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare the non-Human attacker");
    advance_to_dig_choice(&mut runner);

    let WaitingFor::DigChoice {
        cards,
        selectable_cards,
        ..
    } = runner.state().waiting_for.clone()
    else {
        unreachable!("advance_to_dig_choice returns only at DigChoice");
    };
    assert!(cards.contains(&human), "Winota must look at the Human card");
    assert!(
        selectable_cards.contains(&human),
        "the Human creature card must be selectable from Winota's Dig"
    );

    runner
        .act(GameAction::SelectCards { cards: vec![human] })
        .expect("put the selected Human onto the battlefield");
    let WaitingFor::EntryAttackTargetChoice { valid_targets, .. } =
        runner.state().waiting_for.clone()
    else {
        panic!("Winota's Human must choose among multiple defenders");
    };
    assert!(valid_targets.contains(&AttackTarget::Player(P1)));
    assert!(valid_targets.contains(&AttackTarget::Player(p2)));
    runner
        .act(GameAction::ChooseEntryAttackTarget {
            target: AttackTarget::Player(p2),
        })
        .expect("choose a different legal defender for Winota's Human");
    runner.advance_until_stack_empty();

    let human_object = runner.state().objects.get(&human).expect("Human object");
    assert!(human_object.tapped, "the selected Human must enter tapped");
    let combat = runner
        .state()
        .combat
        .as_ref()
        .expect("combat remains active");
    let human_attack = combat
        .attackers
        .iter()
        .find(|attacker_info| attacker_info.object_id == human)
        .expect("the selected Human must enter attacking");
    assert_eq!(human_attack.defending_player, p2);
}
