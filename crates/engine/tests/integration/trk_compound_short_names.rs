//! TRK compound printed short names (CR 201.5c).
//!
//! Captain James T. Kirk and Captain Kathryn Janeway use first-and-last-word
//! self-references in their printed Oracle text. These tests use the verbatim
//! Oracle through `parse_oracle_text` and the live scenario pipeline so reverting
//! the shared normalizer makes both parser shape and gameplay fail.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ControllerRef, Effect, FilterProp, ModalSelectionConstraint, TargetFilter,
    TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const KIRK_ORACLE: &str = "Whenever Captain Kirk enters or attacks, choose one. If you have no cards in hand, choose one or more instead.\n• Discard a card, then draw a card.\n• Create a 1/1 red Officer creature token.\n• Creatures you control get +1/+0 until end of turn.";

const JANEWAY_ORACLE: &str = "You may play an additional land on each of your turns.\nWhenever Captain Janeway or another creature you control enters, that creature explores. (Reveal the top card of your library. Put that card into your hand if it's a land. Otherwise, put a +1/+1 counter on the creature, then put the card back or put it into your graveyard.)";

fn ability_contains_explore(ability: &AbilityDefinition) -> bool {
    matches!(*ability.effect, Effect::Explore)
        || ability
            .sub_ability
            .as_deref()
            .is_some_and(ability_contains_explore)
        || ability
            .else_ability
            .as_deref()
            .is_some_and(ability_contains_explore)
        || ability.mode_abilities.iter().any(ability_contains_explore)
}

#[test]
fn kirk_full_oracle_parses_modal_enters_or_attacks_trigger() {
    let parsed = parse_oracle_text(
        KIRK_ORACLE,
        "Captain James T. Kirk",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Soldier".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|trigger| trigger.mode == TriggerMode::EntersOrAttacks)
        .expect("Captain Kirk must parse as one EntersOrAttacks trigger");
    assert_eq!(trigger.valid_card, Some(TargetFilter::SelfRef));
    let execute = trigger.execute.as_ref().expect("Kirk trigger execute");
    let modal = execute.modal.as_ref().expect("Kirk trigger modal");
    assert_eq!((modal.min_choices, modal.max_choices), (1, 1));
    assert_eq!(modal.mode_count, 3);
    assert_eq!(execute.mode_abilities.len(), 3);
    assert!(modal.constraints.iter().any(|constraint| matches!(
        constraint,
        ModalSelectionConstraint::ConditionalMaxChoices {
            max_choices: 3,
            otherwise_max_choices: 1,
            ..
        }
    )));
    assert!(matches!(
        *execute.mode_abilities[0].effect,
        Effect::Discard { .. }
    ));
    assert!(matches!(
        *execute.mode_abilities[1].effect,
        Effect::Token { .. }
    ));
    assert!(matches!(
        *execute.mode_abilities[2].effect,
        Effect::PumpAll { .. }
    ));
    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "verbatim Kirk Oracle must contain no partial-support placeholder"
    );
}

#[test]
fn janeway_full_oracle_parses_compound_subject_explore() {
    let parsed = parse_oracle_text(
        JANEWAY_ORACLE,
        "Captain Kathryn Janeway",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Scientist".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|trigger| trigger.mode == TriggerMode::ChangesZone)
        .expect("Captain Janeway must parse as an enters trigger");
    let TargetFilter::Or { filters } = trigger
        .valid_card
        .as_ref()
        .expect("Janeway trigger subject")
    else {
        panic!("Janeway trigger must retain its compound subject");
    };
    assert!(filters.contains(&TargetFilter::SelfRef));
    assert!(filters.iter().any(|filter| matches!(
        filter,
        TargetFilter::Typed(typed)
            if typed.type_filters.contains(&TypeFilter::Creature)
                && typed.controller == Some(ControllerRef::You)
                && typed.properties.contains(&FilterProp::Another)
    )));
    assert!(
        trigger
            .execute
            .as_deref()
            .is_some_and(ability_contains_explore),
        "Janeway's compound-subject trigger must execute Explore"
    );
    assert!(
        !format!("{parsed:?}").contains("Unimplemented"),
        "verbatim Janeway Oracle must contain no partial-support placeholder"
    );
}

fn drive_to_kirk_modal(runner: &mut GameRunner) -> (usize, usize, usize) {
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::AbilityModeChoice { modal, .. } => {
                return (modal.min_choices, modal.max_choices, modal.mode_count);
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while driving Kirk trigger");
            }
            other => panic!("unexpected prompt while driving Kirk trigger: {other:?}"),
        }
    }
    panic!("Kirk trigger did not reach AbilityModeChoice");
}

fn add_kirk_to_hand(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "Captain James T. Kirk", 3, 3, KIRK_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id()
}

#[test]
fn kirk_enters_branch_offers_the_live_modal_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_to_hand(P0, "Spare Card", 1, 1);
    let kirk = add_kirk_to_hand(&mut scenario);
    let mut runner = scenario.build();

    let card_id = runner.state().objects[&kirk].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: kirk,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Kirk");

    assert_eq!(
        drive_to_kirk_modal(&mut runner),
        (1, 1, 3),
        "Kirk entering with a card in hand must offer exactly one of three modes"
    );
}

#[test]
fn kirk_attacks_branch_offers_the_same_live_modal_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_to_hand(P0, "Spare Card", 1, 1);
    let kirk = scenario
        .add_creature_from_oracle(P0, "Captain James T. Kirk", 3, 3, KIRK_ORACLE)
        .id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(kirk, AttackTarget::Player(P1))])
        .expect("declare Kirk as an attacker");

    assert_eq!(
        drive_to_kirk_modal(&mut runner),
        (1, 1, 3),
        "Kirk attacking must route through the same EntersOrAttacks modal"
    );
}

#[test]
fn janeway_other_creature_entry_explores_that_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nonland = scenario
        .add_spell_to_library_top(P0, "Explore Probe", true)
        .id();
    let janeway = scenario
        .add_creature_from_oracle(P0, "Captain Kathryn Janeway", 2, 3, JANEWAY_ORACLE)
        .id();
    let explorer = scenario.add_creature_to_hand(P0, "Away Team", 2, 2).id();
    let mut runner = scenario.build();

    runner.cast(explorer).resolve();

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DigChoice { ref cards, .. } if cards == &vec![nonland]
    ));
    runner
        .act(GameAction::SelectCards { cards: vec![] })
        .expect("put the explored nonland into the graveyard");

    assert_eq!(runner.state().objects[&nonland].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().objects[&explorer]
            .counters
            .values()
            .copied()
            .sum::<u32>(),
        1,
        "the entering Away Team must receive the explore counter"
    );
    assert_eq!(
        runner.state().objects[&janeway]
            .counters
            .values()
            .copied()
            .sum::<u32>(),
        0,
        "Janeway must not receive Away Team's explore counter"
    );
}
