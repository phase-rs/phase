use engine::ai_support::candidate_actions;
use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
    QuantityExpr, SacrificeCost, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, ManaChoice, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

fn sacrificial_mana_ability(cost: AbilityCost, color: ManaColor) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![color],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(cost)
}

fn any_one_color_sacrificial_mana_ability(cost: AbilityCost) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::AnyOneColor {
                count: QuantityExpr::Fixed { value: 1 },
                color_options: vec![ManaColor::Black, ManaColor::Red],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(cost)
}

fn begin_sacrificial_payment(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::AutoExceptSacrificialMana,
        })
        .expect("the production cast path should stop before sacrificial mana");
}

fn offered_selection(
    runner: &GameRunner,
    source: ObjectId,
) -> engine::types::mana::ManaSourceSelection {
    let WaitingFor::ManaSourceSelection { options, .. } = &runner.state().waiting_for else {
        panic!(
            "expected sacrificial mana prompt from the cast path, got {:?}",
            runner.state().waiting_for
        );
    };
    options
        .iter()
        .find(|selection| selection.source.object_id == source)
        .cloned()
        .expect("the prompt should retain the sacrificial source's exact capability")
}

fn generic_spell(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand(P0, "Sacrificial Mana Payment Witness", true)
        .with_mana_cost(ManaCost::generic(1))
        .id()
}

/// A self-sacrificing mana ability is offered only after the real cast pipeline
/// has exhausted non-sacrificial payment rows.
#[test]
fn self_sacrificing_mana_source_pays_a_production_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let source = scenario
        .add_creature(P0, "Blood Pet Witness", 1, 1)
        .with_ability_definition(sacrificial_mana_ability(
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ManaColor::Black,
        ))
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);
    let selection = offered_selection(&runner, source);
    runner
        .act(GameAction::ActivateManaSource { selection })
        .expect("the offered self-sacrificing source should activate during payment");

    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Black),
        1,
        "the activated source's mana reaches the pending spell payment"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));

    runner
        .act(GameAction::PassPriority)
        .expect("the selected mana should pay the spell through the ordinary payment reducer");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

/// A frozen source selection must preserve an AnyOneColor activation's normal
/// color prompt, then resume and finish the original cast.
#[test]
fn any_one_color_self_sacrifice_selection_completes_production_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let source = scenario
        .add_creature(P0, "Gold Witness", 1, 1)
        .with_ability_definition(any_one_color_sacrificial_mana_ability(
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
        ))
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);
    let selection = offered_selection(&runner, source);
    runner
        .act(GameAction::ActivateManaSource { selection })
        .expect("the frozen self-sacrifice selection should enter the color prompt");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseManaColor { .. }
    ));

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(ManaType::Red),
            count: 1,
        })
        .expect("choosing the source's color should resume the pending payment");
    assert_eq!(runner.state().objects[&source].zone, Zone::Graveyard);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));

    runner
        .act(GameAction::PassPriority)
        .expect("the chosen mana should finish the original cast");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

/// A non-sacrificial row on the same permanent remains available to the
/// automatic planner; only the irreversible row is held for explicit consent.
#[test]
fn automatic_payment_keeps_a_non_sacrificial_row_on_a_sacrificial_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let source = scenario
        .add_creature(P0, "Two-Row Mana Witness", 1, 1)
        .as_artifact()
        .with_ability_definition(sacrificial_mana_ability(AbilityCost::Tap, ManaColor::Red))
        .with_ability_definition(sacrificial_mana_ability(
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ManaColor::Black,
        ))
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().objects[&source].tapped);
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&spell].zone, Zone::Stack);
}

/// The pre-activation prompt does not bypass a mana ability's own sacrifice
/// choice; selecting another artifact pays that cost while its source remains
/// on the battlefield.
#[test]
fn sacrifice_another_permanent_mana_source_resumes_the_pending_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let source = scenario
        .add_creature(P0, "Krark-Clan Ironworks Witness", 1, 1)
        .as_artifact()
        .with_ability_definition(sacrificial_mana_ability(
            AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                1,
            )),
            ManaColor::Red,
        ))
        .id();
    let sacrifice = scenario
        .add_creature(P0, "Sacrificial Artifact Witness", 1, 1)
        .as_artifact()
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);
    let selection = offered_selection(&runner, source);
    runner
        .act(GameAction::ActivateManaSource { selection })
        .expect("the offered source should enter its normal interactive cost payment");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            ref choices,
            ..
        } if choices.contains(&sacrifice)
    ));

    runner
        .act(GameAction::SelectCards {
            cards: vec![sacrifice],
        })
        .expect("the source's ordinary sacrifice-cost reducer should accept another artifact");

    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&sacrifice].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        1,
        "the selected ability's mana remains available to the original spell"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
}

/// Returning from the safety prompt keeps the pending spell but neither spends
/// mana nor performs the irreversible source activation.
#[test]
fn back_from_sacrificial_mana_prompt_preserves_the_cast_without_cancellation() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let source = scenario
        .add_creature(P0, "Back Button Witness", 1, 1)
        .with_ability_definition(sacrificial_mana_ability(
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ManaColor::Black,
        ))
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);
    assert!(
        !candidate_actions(runner.state())
            .iter()
            .any(|candidate| matches!(candidate.action, GameAction::CancelCast)),
        "the real safety prompt must not synthesize a cast-cancellation action"
    );
    runner
        .act(GameAction::BackToManaPayment)
        .expect("the prompt's explicit back action should be accepted");

    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    assert!(runner.state().pending_cast.is_some());
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
}

/// An engine-authored selection is revalidated at the reducer boundary, so a
/// source that became ineligible cannot be activated from a stale payment prompt.
#[test]
fn stale_sacrificial_mana_selection_is_rejected_without_mutating_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = generic_spell(&mut scenario);
    let source = scenario
        .add_creature(P0, "Stale Selection Witness", 1, 1)
        .with_ability_definition(sacrificial_mana_ability(
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ManaColor::Black,
        ))
        .id();
    let mut runner = scenario.build();

    begin_sacrificial_payment(&mut runner, spell);
    let selection = offered_selection(&runner, source);
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .controller = PlayerId(1);

    assert!(
        runner
            .act(GameAction::ActivateManaSource { selection })
            .is_err(),
        "a stale source must fail before the payment reducer mutates state"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaSourceSelection { .. }
    ));
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&source].controller, PlayerId(1));
    assert_eq!(runner.state().players[P0.0 as usize].mana_pool.total(), 0);
}
