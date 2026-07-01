use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const TOUGHNESS_X_OR_LESS: &str = "Destroy target creature with toughness X or less.";

#[test]
fn bare_chosen_x_ptcomparison_defers_target_selection_until_x_is_chosen() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "X Toughness Test", true, TOUGHNESS_X_OR_LESS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X],
            generic: 0,
        })
        .id();
    let low_toughness = scenario.add_creature(P1, "Low Toughness", 1, 2).id();
    let other_low_toughness = scenario.add_creature(P1, "Other Low Toughness", 2, 2).id();
    let high_toughness = scenario.add_creature(P1, "High Toughness", 1, 3).id();
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(9_000), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(9_001), false, vec![]),
        ],
    );

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting should ask for X before target enumeration");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }),
        "bare X in a PtComparison target filter must choose X before target selection, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::ChooseX { value: 2 })
        .expect("choosing X should enumerate target slots");

    let WaitingFor::TargetSelection { target_slots, .. } = &runner.state().waiting_for else {
        panic!(
            "expected TargetSelection after choosing X, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(target_slots.len(), 1);
    assert!(
        target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(low_toughness)),
        "X=2 should include toughness-2 creature: {:?}",
        target_slots[0].legal_targets
    );
    assert!(
        target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(other_low_toughness)),
        "X=2 should include the other toughness-2 creature: {:?}",
        target_slots[0].legal_targets
    );
    assert!(
        !target_slots[0]
            .legal_targets
            .contains(&TargetRef::Object(high_toughness)),
        "X=2 should exclude toughness-3 creature: {:?}",
        target_slots[0].legal_targets
    );

    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(low_toughness)],
        })
        .expect("legal X-filtered target should be accepted");
}
