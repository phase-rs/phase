use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::Effect;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const ORACLE: &str = "When A Girl and Her Dogs enters the battlefield and at the beginning of your upkeep, create a 1/1 white legendary Dog creature token and name it.\nWhenever A Girl and Her Dogs attacks, it gets +1/+1 until end of turn for each legendary creature you control.";

#[test]
fn a_girl_and_her_dogs_fences_token_creation_with_player_assigned_name() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let girl = scenario
        .add_creature_to_hand_from_oracle(P0, "A Girl and Her Dogs", 3, 3, ORACLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    let mut runner = scenario.build();

    runner.cast(girl).commit();
    runner.pass_both_players();

    assert_eq!(runner.state().stack.len(), 1, "expected the enters trigger");
    let trigger = &runner.state().stack[0];
    assert!(matches!(
        trigger.kind,
        StackEntryKind::TriggeredAbility { .. }
    ));
    assert_eq!(trigger.source_id, girl);
    assert!(
        matches!(
            &trigger.ability().expect("trigger ability").effect,
            Effect::Unimplemented { name, description: Some(fragment) }
                if name == "name_created_token" && fragment.trim_end_matches('.').ends_with("and name it")
        ),
        "expected an explicit token-naming gap, got {:?}",
        trigger.ability()
    );

    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().battlefield.len(), 1);
    assert!(runner.state().battlefield.contains(&girl));
    assert!(
        !runner
            .state()
            .battlefield_entries_this_turn
            .iter()
            .any(|entry| entry.name == "Dog"),
        "the fenced instruction must not create a Dog even transiently"
    );
}
