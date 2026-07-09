//! Runtime regression for #3270 — Extinction Event's odd/even choice.
//!
//! "Choose odd or even. Exile each creature with mana value of the chosen
//! quality."

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const EXTINCTION_EVENT: &str =
    "Choose odd or even. Exile each creature with mana value of the chosen quality.";

#[test]
fn extinction_event_exiles_only_the_chosen_mana_value_parity() {
    let mut scenario = GameScenario::new_n_player(2, 3270);
    scenario.at_phase(Phase::PreCombatMain);

    let odd_creature = scenario
        .add_creature(P0, "Odd Creature", 3, 3)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let even_creature = scenario
        .add_creature(P1, "Even Creature", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let zero_creature = scenario
        .add_creature(P1, "Zero Creature", 1, 1)
        .with_mana_cost(ManaCost::zero())
        .id();

    let extinction_event = scenario
        .add_spell_to_hand_from_oracle(P0, "Extinction Event", false, EXTINCTION_EVENT)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    runner.cast(extinction_event).resolve();
    let WaitingFor::NamedChoice { options, .. } = &runner.state().waiting_for else {
        panic!(
            "Extinction Event must pause on odd/even choice, got {}",
            runner.waiting_for_kind()
        );
    };
    assert!(options.iter().any(|choice| choice == "Odd"));
    assert!(options.iter().any(|choice| choice == "Even"));

    runner
        .act(GameAction::ChooseOption {
            choice: "Odd".to_string(),
        })
        .expect("ChooseOption(Odd) must resolve");
    runner.advance_until_stack_empty();

    let zone_of = |id| runner.state().objects[&id].zone;

    // CR 202.3 + CR 608.2c: the chosen odd/even quality filters mana value.
    assert_eq!(zone_of(odd_creature), Zone::Exile);
    assert_eq!(zone_of(even_creature), Zone::Battlefield);
    assert_eq!(zone_of(zero_creature), Zone::Battlefield);
}
