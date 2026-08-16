//! Issue #4376: ordinary activated stack entries retain their Oracle
//! description while carrying no modal-choice labels.
//!
//! Elspeth Resplendent's −7 is deliberately a nonmodal guard: it proves the
//! new display field doesn't replace an ordinary activated ability description.
//! It does not validate modal-label propagation.

use engine::game::derived_views::{ClientGameState, ClientGameStateRef};
use engine::game::filter_state_for_viewer;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const ELSPETH_MINUS_SEVEN_DESCRIPTION: &str =
    "[−7]: Create five 3/3 white Angel creature tokens with flying.";

fn client_state(runner: &GameRunner) -> ClientGameState {
    let filtered = filter_state_for_viewer(runner.state(), P0);
    let json = serde_json::to_string(&ClientGameStateRef::wrap_filtered(
        runner.state(),
        &filtered,
        Some(P0),
    ))
    .expect("serialize Elspeth stack display");
    serde_json::from_str(&json).expect("deserialize Elspeth stack display")
}

#[test]
fn elspeth_resplendent_minus_seven_keeps_description_without_mode_labels() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let elspeth = scenario.add_real_card(P0, "Elspeth Resplendent", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let minus_seven_index = runner.state().objects[&elspeth]
        .abilities
        .iter()
        .position(|ability| {
            matches!(
                ability.cost.as_ref(),
                Some(AbilityCost::Loyalty { amount: -7 })
            )
        })
        .expect("Elspeth Resplendent must expose its nonmodal −7 loyalty ability");
    {
        let elspeth = runner
            .state_mut()
            .objects
            .get_mut(&elspeth)
            .expect("Elspeth remains on the battlefield");
        // CR 306.5b: seed the planeswalker's displayed loyalty and its loyalty
        // counter count together for this pre-existing battlefield fixture.
        elspeth.loyalty = Some(7);
        elspeth.counters.insert(CounterType::Loyalty, 7);
    }

    // CR 606.6: the −7 cost is legal only with at least seven loyalty counters.
    runner
        .act(GameAction::ActivateAbility {
            source_id: elspeth,
            ability_index: minus_seven_index,
        })
        .expect("Elspeth's −7 activation reaches the stack");

    let stack_entry_id = runner
        .state()
        .stack
        .back()
        .expect("Elspeth's activated ability remains on the stack")
        .id;
    let client_state = client_state(&runner);
    let stack_entry = client_state
        .derived
        .stack_entry_details
        .get(&stack_entry_id)
        .expect("Elspeth's activated ability is publicly displayed on the stack");
    assert_eq!(
        stack_entry.ability_description.as_deref(),
        Some(ELSPETH_MINUS_SEVEN_DESCRIPTION),
        "ordinary activated abilities retain their Oracle stack description"
    );
    assert!(
        stack_entry.selected_mode_labels.is_empty(),
        "nonmodal Elspeth's −7 must not publish selected mode labels"
    );

    runner.advance_until_stack_empty();

    // CR 111.4: this token-producing ability names its Angel tokens.
    let angel_tokens: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|object| object.is_token && object.name == "Angel")
        .collect();
    assert_eq!(
        angel_tokens.len(),
        5,
        "Elspeth's −7 must create five Angel tokens"
    );
}
