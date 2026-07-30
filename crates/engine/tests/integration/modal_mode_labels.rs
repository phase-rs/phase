use engine::game::derived_views::{ClientGameState, ClientGameStateRef};
use engine::game::filter_state_for_viewer;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, StackEntryKind, WaitingFor};
use engine::types::phase::Phase;

const BROTHERHOODS_END: &str = "Choose one —\n\
• Brotherhood's End deals 3 damage to each creature and each planeswalker.\n\
• Destroy all artifacts with mana value 3 or less.";

fn opponent_client_state(runner: &GameRunner) -> ClientGameState {
    let filtered = filter_state_for_viewer(runner.state(), P1);
    let json = serde_json::to_string(&ClientGameStateRef::wrap_filtered(
        runner.state(),
        &filtered,
        Some(P1),
    ))
    .expect("serialize opponent state");
    serde_json::from_str(&json).expect("deserialize opponent state")
}

#[test]
fn brotherhoods_end_publishes_selected_mode_label_only_after_selection() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Brotherhood's End", false, BROTHERHOODS_END)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Brotherhood's End reaches its mode choice");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ModeChoice { .. }
    ));
    let StackEntryKind::Spell { ability, .. } = &runner.state().stack.back().unwrap().kind else {
        panic!("Brotherhood's End must be represented by a spell stack entry");
    };
    assert!(
        ability.is_none(),
        "the raw entry remains unfinalized before mode selection"
    );
    assert!(
        opponent_client_state(&runner)
            .derived
            .stack_entry_details
            .get(&spell)
            .expect("opponent sees the public stack entry")
            .selected_mode_labels
            .is_empty(),
        "no selected label may be published before a mode is chosen",
    );

    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .expect("select Brotherhood's End artifact mode");
    let StackEntryKind::Spell {
        ability: Some(ability),
        ..
    } = &runner.state().stack.back().unwrap().kind
    else {
        panic!("mode selection must finalize the spell ability on the stack");
    };
    assert_eq!(
        ability.selected_mode_labels,
        ["Destroy all artifacts with mana value 3 or less."],
        "the finalized stack ability retains the exact selected Oracle mode",
    );
    assert_eq!(
        opponent_client_state(&runner)
            .derived
            .stack_entry_details
            .get(&spell)
            .expect("opponent keeps the public stack entry")
            .selected_mode_labels,
        ["Destroy all artifacts with mana value 3 or less."],
        "the public selected label survives opponent filtering and client wrapping",
    );
}
