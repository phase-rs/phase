//! CR 113.6j: Onakke Oathkeeper's graveyard activation must use its printed
//! activation zone and pay both parts of its composite cost through the normal
//! action pipeline.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ONAKKE_OATHKEEPER_ORACLE: &str = "Creatures can't attack planeswalkers you control unless their controller pays {1} for each creature they control that's attacking a planeswalker you control.\n{4}{W}{W}, Exile this card from your graveyard: Return target planeswalker card from your graveyard to the battlefield.";

fn mana(count: usize, mana_type: ManaType) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(mana_type, ObjectId(0), false, vec![]))
        .collect()
}

/// CR 602.2b / CR 115.1c / CR 601.2c-h: the printed graveyard activation
/// announces its target before paying `{4}{W}{W}` and exiling its source, then
/// can return only its controller's planeswalker card from that graveyard.
#[test]
fn onakke_oathkeeper_graveyard_activation_returns_own_planeswalker() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let oathkeeper = scenario
        .add_creature_from_oracle(P0, "Onakke Oathkeeper", 2, 2, ONAKKE_OATHKEEPER_ORACLE)
        .id();
    let own_planeswalker = scenario
        .add_creature(P0, "Own Jace", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 4)
        .id();
    // A second legal target prevents the activation pipeline from
    // auto-selecting the only available planeswalker.
    let other_own_planeswalker = scenario
        .add_creature(P0, "Other Jace", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 4)
        .id();
    let opponent_planeswalker = scenario
        .add_creature(P1, "Opponent Chandra", 0, 0)
        .as_planeswalker_with_loyalty("Chandra", 4)
        .id();
    scenario.with_mana_pool(
        P0,
        mana(4, ManaType::Colorless)
            .into_iter()
            .chain(mana(2, ManaType::White))
            .collect(),
    );
    let mut runner = scenario.build();
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), oathkeeper, Zone::Graveyard, &mut events);
    move_to_zone(
        runner.state_mut(),
        own_planeswalker,
        Zone::Graveyard,
        &mut events,
    );
    move_to_zone(
        runner.state_mut(),
        other_own_planeswalker,
        Zone::Graveyard,
        &mut events,
    );
    move_to_zone(
        runner.state_mut(),
        opponent_planeswalker,
        Zone::Graveyard,
        &mut events,
    );

    runner
        .act(GameAction::ActivateAbility {
            source_id: oathkeeper,
            ability_index: 0,
        })
        .expect("Onakke Oathkeeper activation must enter target selection");

    // CR 602.2b + CR 601.2c: target legality is fixed before costs are paid.
    let WaitingFor::TargetSelection {
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected target selection after activation, got {:?}",
            runner.state().waiting_for
        );
    };
    let legal_targets = &target_slots[selection.current_slot].legal_targets;
    assert!(
        legal_targets.contains(&TargetRef::Object(own_planeswalker)),
        "the controller's planeswalker card must be a legal target"
    );
    assert!(
        legal_targets.contains(&TargetRef::Object(other_own_planeswalker)),
        "the controller's other planeswalker card must be a legal target"
    );
    assert!(
        !legal_targets.contains(&TargetRef::Object(opponent_planeswalker)),
        "the opponent's planeswalker card must not be a legal target"
    );

    let before_illegal_target = runner.state().clone();
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(opponent_planeswalker)),
        })
        .expect_err("choosing the opponent's planeswalker must be rejected");
    assert_eq!(
        runner.state(),
        &before_illegal_target,
        "an illegal target choice must leave the game state unchanged"
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(own_planeswalker)),
        })
        .expect("choosing the controller's planeswalker must be accepted");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ),
        "the unambiguous mana pool must auto-pay the activation and return priority"
    );
    assert_eq!(
        runner.state().objects[&oathkeeper].zone,
        Zone::Exile,
        "the self-exile cost must be paid before the activated ability is put on the stack"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the fully paid targeted activation must be waiting on the stack"
    );

    // CR 602.2b + CR 601.2h: with an unambiguous pool, the production payment
    // pipeline automatically pays the mana leg and self-exile cost, then priority
    // resumes for the ability already on the stack.
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&oathkeeper].zone, Zone::Exile);
    assert_eq!(
        runner.state().objects[&own_planeswalker].zone,
        Zone::Battlefield
    );
    assert_eq!(
        runner.state().objects[&other_own_planeswalker].zone,
        Zone::Graveyard,
        "the unchosen controller's planeswalker remains in the graveyard"
    );
    assert_eq!(
        runner.state().objects[&opponent_planeswalker].zone,
        Zone::Graveyard,
        "the opponent's planeswalker was never a legal target"
    );
}
