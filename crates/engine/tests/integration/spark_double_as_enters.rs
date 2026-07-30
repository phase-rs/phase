//! Spark Double preserves its printed entry counter while applying a copied
//! as-enters choice.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SPARK_DOUBLE: &str = "You may have this creature enter as a copy of a creature or planeswalker you control, except it enters with an additional +1/+1 counter on it if it's a creature, it enters with an additional loyalty counter on it if it's a planeswalker, and it isn't legendary.";
const PAINTERS_SERVANT: &str = "As this creature enters, choose a color.\nAll cards that aren't on the battlefield, spells, and permanents are the chosen color in addition to their other colors.";

#[test]
fn spark_double_copying_painters_servant_keeps_its_counter_and_prompts_for_color_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let painter = scenario
        .add_creature_from_oracle(P0, "Painter's Servant", 1, 3, PAINTERS_SERVANT)
        .id();
    let spark = scenario
        .add_creature_to_hand_from_oracle(P0, "Spark Double", 0, 0, SPARK_DOUBLE)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spark].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spark,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Spark Double");
    runner.advance_until_stack_empty();

    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!(
            "Spark Double must offer its optional enter-as-copy replacement, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(candidates.len(), 2);
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Spark Double's copy replacement");

    let WaitingFor::CopyTargetChoice { valid_targets, .. } = &runner.state().waiting_for else {
        panic!(
            "Spark Double must offer a copy target, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(valid_targets.contains(&painter));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(painter)),
        })
        .expect("copy Painter's Servant");

    let WaitingFor::NamedChoice { options, .. } = &runner.state().waiting_for else {
        panic!(
            "the copied Painter's Servant must offer its as-enters color choice, got {:?}",
            runner.state().waiting_for
        );
    };
    let blue = options
        .iter()
        .find(|option| option.eq_ignore_ascii_case("blue"))
        .expect("Painter's Servant offers blue")
        .clone();
    runner
        .act(GameAction::ChooseOption { choice: blue })
        .expect("choose blue for the copied Painter's Servant");

    let entered = &runner.state().objects[&spark];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(entered.name, "Painter's Servant");
    assert_eq!(entered.chosen_color(), Some(ManaColor::Blue));
    assert_eq!(
        entered.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1),
        "CR 614.12 + CR 707.9: Spark Double's printed entry replacement must add exactly one counter while the copied as-enters choice applies once"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}
