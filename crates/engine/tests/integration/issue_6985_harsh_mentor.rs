use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const HARSH_MENTOR: &str = "Whenever an opponent activates an ability of an artifact, creature, or land on the battlefield, if it isn't a mana ability, this creature deals 2 damage to that player.";
const NONMANA_ABILITY: &str = "{T}: Draw a card.";
const MANA_ABILITY: &str = "{T}: Add {G}.";

fn activate_after_p0_passes(oracle: &str, controller: PlayerId) -> i32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest"]);
    scenario.with_library_top(P1, &["Forest"]);
    scenario
        .add_creature(P0, "Harsh Mentor", 2, 2)
        .from_oracle_text(HARSH_MENTOR);
    let source = scenario
        .add_creature(controller, "Activation Source", 1, 1)
        .from_oracle_text(oracle)
        .id();

    let mut runner = scenario.build();
    if controller == P1 {
        runner
            .act(GameAction::PassPriority)
            .expect("P0 passes priority to P1");
    }
    runner.activate(source, 0).resolve().life_delta(controller)
}

/// CR 603.2 + CR 605.3b: Harsh Mentor observes a nonmana ability activated
/// by an opponent, but mana abilities do not use the stack and are excluded.
#[test]
fn harsh_mentor_damages_an_opponent_for_a_nonmana_creature_ability() {
    assert_eq!(
        activate_after_p0_passes(NONMANA_ABILITY, P1),
        -2,
        "a stack-using activated ability of an opponent's creature triggers Harsh Mentor"
    );
}

#[test]
fn harsh_mentor_excludes_mana_abilities_and_its_controller() {
    assert_eq!(
        activate_after_p0_passes(MANA_ABILITY, P1),
        0,
        "a mana ability must not emit the activation trigger event"
    );
    assert_eq!(
        activate_after_p0_passes(NONMANA_ABILITY, P0),
        0,
        "Harsh Mentor only observes opponents' activations"
    );
}
