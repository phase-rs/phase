use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KILI: &str = "Storied (If you control three or more artifacts, legendaries, and/or Sagas, you have an enduring story for the rest of the game.)\nAs long as you have an enduring story, you may pay {0} rather than pay the equip cost of the first equip ability you activate each turn.\nWhenever another Dwarf or Equipment you control enters, draw a card. This ability triggers only once each turn.";

fn add_generic_mana(runner: &mut engine::game::scenario::GameRunner, amount: u32) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|player| player.id == P0)
        .unwrap()
        .mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

fn activate_equip(
    runner: &mut engine::game::scenario::GameRunner,
    equipment: ObjectId,
) -> engine::types::game_state::ActionResult {
    runner
        .act(GameAction::ActivateAbility {
            source_id: equipment,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect(&format!(
            "Equip activation should be legal for {equipment:?} with mana {}",
            runner.state().players[0].mana_pool.total()
        ))
}

fn advance_to_postcombat(runner: &mut engine::game::scenario::GameRunner) {
    runner.advance_to_phase(Phase::PostCombatMain);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner
            .declare_attackers(&[])
            .expect("an empty attack declaration should advance the turn");
        runner.advance_to_phase(Phase::PostCombatMain);
    }
}

#[test]
fn kili_alternative_equip_cost_is_story_gated_and_first_per_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kili = scenario
        .add_creature_from_oracle(P0, "Kíli the Resourceful", 2, 2, KILI)
        .as_legendary()
        .id();
    let first_equipment = scenario
        .add_artifact_from_oracle(P0, "First Equipment", "Equip {2}")
        .with_subtypes(vec!["Equipment"])
        .id();
    let second_equipment = scenario
        .add_artifact_from_oracle(P0, "Second Equipment", "Equip {2}")
        .with_subtypes(vec!["Equipment"])
        .id();
    let third_equipment = scenario
        .add_artifact_from_oracle(P0, "Third Equipment", "Equip {2}")
        .with_subtypes(vec!["Equipment"])
        .id();
    scenario.with_library_top(P0, &["P0 Draw"]);
    scenario.with_library_top(P1, &["P1 Draw"]);

    let mut runner = scenario.build();
    advance_to_postcombat(&mut runner);
    assert!(
        runner.state().enduring_story.contains(&P0),
        "Kíli and two Equipment artifacts should establish an enduring story after state checks"
    );

    let result = activate_equip(&mut runner, first_equipment);
    let zero_cost_branch = match result.waiting_for {
        WaitingFor::ActivationCostOneOfChoice { ref costs, .. } => {
            assert_eq!(costs.len(), 1, "the unaffordable {{2}} branch is filtered");
            costs
                .iter()
                .position(|cost| {
                    matches!(
                        cost,
                        AbilityCost::Mana { cost } if cost == &ManaCost::generic(0)
                    )
                })
                .expect("the active story should offer the {0} Equip branch")
        }
        other => panic!("expected the story-gated alternative cost choice, got {other:?}"),
    };
    runner
        .act(GameAction::ChooseActivationCostBranch {
            index: zero_cost_branch,
        })
        .expect("choosing the {0} branch should be accepted");
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&kili]
            .attachments
            .contains(&first_equipment),
        "the first Equipment should attach after paying the alternative cost"
    );

    add_generic_mana(&mut runner, 2);
    let second = activate_equip(&mut runner, second_equipment);
    assert!(
        !matches!(
            second.waiting_for,
            WaitingFor::ActivationCostOneOfChoice { .. }
        ),
        "the alternative applies only to the first Equip activation each turn"
    );
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&kili]
            .attachments
            .contains(&second_equipment),
        "the second Equipment should still equip normally"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "the second activation pays its ordinary {{2}} equip cost"
    );

    runner.advance_to_phase(Phase::PreCombatMain);
    if runner.state().active_player != P0 {
        advance_to_postcombat(&mut runner);
        runner.advance_to_phase(Phase::PreCombatMain);
    }
    assert_eq!(
        runner.state().active_player,
        P0,
        "expected Kíli's next turn; phase={:?}, waiting={:?}, turn={}",
        runner.state().phase,
        runner.state().waiting_for,
        runner.state().turn_number
    );
    let next_turn = activate_equip(&mut runner, third_equipment);
    assert!(
        matches!(
            next_turn.waiting_for,
            WaitingFor::ActivationCostOneOfChoice { ref costs, .. }
                if costs.iter().any(|cost| matches!(
                    cost,
                    AbilityCost::Mana { cost } if cost == &ManaCost::generic(0)
                ))
        ),
        "the alternative cost becomes available again on the next turn"
    );
}

#[test]
fn kili_uses_the_normal_equip_cost_without_an_enduring_story() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kili = scenario
        .add_creature_from_oracle(P0, "Kíli the Resourceful", 2, 2, KILI)
        .as_legendary()
        .id();
    let equipment = scenario
        .add_artifact_from_oracle(P0, "Equipment", "Equip {2}")
        .with_subtypes(vec!["Equipment"])
        .id();

    let mut runner = scenario.build();
    advance_to_postcombat(&mut runner);
    assert!(
        !runner.state().enduring_story.contains(&P0),
        "Kíli and the Equipment alone are below the Storied threshold"
    );
    add_generic_mana(&mut runner, 2);
    let result = activate_equip(&mut runner, equipment);
    assert!(
        !matches!(
            result.waiting_for,
            WaitingFor::ActivationCostOneOfChoice { .. }
        ),
        "the alternative branch must not be offered without an enduring story"
    );
    runner.advance_until_stack_empty();
    assert!(
        runner.state().objects[&kili]
            .attachments
            .contains(&equipment),
        "the ordinary equip ability must still attach the Equipment"
    );
    assert_eq!(
        runner.state().objects[&equipment].zone,
        Zone::Battlefield,
        "paying the normal Equip cost leaves the Equipment on the battlefield"
    );
}
