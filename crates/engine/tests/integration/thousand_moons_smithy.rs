//! End-to-end coverage for Thousand Moons Smithy's reflexive optional payment.

use std::sync::Arc;

use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::trigger_index::reindex_object_triggers;
use engine::types::ability::{Effect, TargetFilter, TriggerDefinition};
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;
use engine::types::PlayerId;

const THOUSAND_MOONS_SMITHY: &str = "When Thousand Moons Smithy enters, create a white Gnome Soldier artifact creature token with \"This token's power and toughness are each equal to the number of artifacts and/or creatures you control.\"\nAt the beginning of your first main phase, you may tap five untapped artifacts and/or creatures you control. If you do, transform Thousand Moons Smithy.";

fn attach_back_face(runner: &mut GameRunner, smithy: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&smithy)
        .unwrap()
        .back_face = Some(BackFaceData {
        name: "Barracks of the Thousand".to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Land],
            subtypes: vec![],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: Some(LayoutKind::Transform),
        parse_warnings: vec![],
    });
}

fn resolve_first_main_trigger(runner: &mut GameRunner) {
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.auto_advance_to_main_phase();
    assert_eq!(runner.state().phase, Phase::PreCombatMain);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Smithy's first-main trigger must reach the stack"
    );
    runner.act(GameAction::PassPriority).unwrap();
    runner.act(GameAction::PassPriority).unwrap();
}

fn assert_tap_five_prompt(runner: &GameRunner, player: PlayerId) {
    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            player: prompt_player,
            kind: PayCostKind::TapCreatures { .. },
            count,
            min_count,
            choices,
            ..
        } => {
            assert_eq!(*prompt_player, player);
            assert_eq!(*count, 5);
            assert_eq!(*min_count, 5);
            assert!(choices.len() >= 5);
        }
        other => panic!("expected Smithy's fixed five-permanent payment prompt, got {other:?}"),
    }
}

#[test]
fn thousand_moons_smithy_taps_exactly_five_and_transforms() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    scenario.with_library_top(P0, &["P0 Library Card"; 40]);
    let smithy = scenario
        .add_creature_from_oracle(P0, "Thousand Moons Smithy", 1, 1, THOUSAND_MOONS_SMITHY)
        .as_artifact()
        .id();
    let payments: Vec<_> = (0..5)
        .map(|_| scenario.add_creature(P0, "Payment", 1, 1).id())
        .collect();
    let mut runner = scenario.build();
    attach_back_face(&mut runner, smithy);

    resolve_first_main_trigger(&mut runner);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    assert_tap_five_prompt(&runner, P0);
    runner
        .act(GameAction::SelectCards {
            cards: payments.clone(),
        })
        .unwrap();

    assert!(runner.state().objects[&smithy].transformed);
    assert!(payments.iter().all(|id| runner.state().objects[id].tapped));
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

#[test]
fn thousand_moons_smithy_decline_and_under_five_skip_the_payment_prompt() {
    let mut decline = GameScenario::new();
    decline.at_phase(Phase::Upkeep);
    decline.with_library_top(P0, &["P0 Library Card"; 40]);
    let decline_smithy = decline
        .add_creature_from_oracle(P0, "Thousand Moons Smithy", 1, 1, THOUSAND_MOONS_SMITHY)
        .as_artifact()
        .id();
    let decline_payments: Vec<_> = (0..5)
        .map(|_| decline.add_creature(P0, "Decline Payment", 1, 1).id())
        .collect();
    let mut decline_runner = decline.build();
    attach_back_face(&mut decline_runner, decline_smithy);
    resolve_first_main_trigger(&mut decline_runner);
    decline_runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .unwrap();
    assert!(!decline_runner.state().objects[&decline_smithy].transformed);
    assert!(decline_payments
        .iter()
        .all(|id| !decline_runner.state().objects[id].tapped));

    let mut under_five = GameScenario::new();
    under_five.at_phase(Phase::Upkeep);
    under_five.with_library_top(P0, &["P0 Library Card"; 40]);
    let under_five_smithy = under_five
        .add_creature_from_oracle(P0, "Thousand Moons Smithy", 1, 1, THOUSAND_MOONS_SMITHY)
        .as_artifact()
        .id();
    for _ in 0..3 {
        under_five.add_creature(P0, "Insufficient Payment", 1, 1);
    }
    let mut under_five_runner = under_five.build();
    attach_back_face(&mut under_five_runner, under_five_smithy);
    resolve_first_main_trigger(&mut under_five_runner);
    assert!(matches!(
        under_five_runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    assert!(under_five_runner.state().stack.is_empty());
    assert!(!under_five_runner.state().objects[&under_five_smithy].transformed);
}

#[test]
fn thousand_moons_smithy_rejects_invalid_fixed_selection_without_losing_the_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    scenario.with_library_top(P0, &["P0 Library Card"; 40]);
    let smithy = scenario
        .add_creature_from_oracle(P0, "Thousand Moons Smithy", 1, 1, THOUSAND_MOONS_SMITHY)
        .as_artifact()
        .id();
    let payments: Vec<_> = (0..6)
        .map(|_| scenario.add_creature(P0, "Payment", 1, 1).id())
        .collect();
    let opponent = scenario.add_creature(P1, "Opponent Payment", 1, 1).id();
    let nonmatching = scenario.add_basic_land(P0, ManaColor::White);
    let mut runner = scenario.build();
    attach_back_face(&mut runner, smithy);
    runner
        .state_mut()
        .objects
        .get_mut(&payments[5])
        .unwrap()
        .tapped = true;
    resolve_first_main_trigger(&mut runner);
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    assert_tap_five_prompt(&runner, P0);
    let invalid_selections = vec![
        payments[..4].to_vec(),
        vec![
            payments[0],
            payments[1],
            payments[2],
            payments[3],
            payments[0],
        ],
        vec![
            payments[0],
            payments[1],
            payments[2],
            payments[3],
            payments[5],
        ],
        vec![payments[0], payments[1], payments[2], payments[3], opponent],
        vec![
            payments[0],
            payments[1],
            payments[2],
            payments[3],
            nonmatching,
        ],
    ];
    for cards in invalid_selections {
        assert!(runner.act(GameAction::SelectCards { cards }).is_err());
        assert_tap_five_prompt(&runner, P0);
        assert!(!runner.state().objects[&smithy].transformed);
    }

    runner
        .act(GameAction::SelectCards {
            cards: payments[..5].to_vec(),
        })
        .unwrap();
    assert!(runner.state().objects[&smithy].transformed);
}

#[test]
fn thousand_moons_smithy_payment_uses_triggering_player_not_source_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P1, &["P1 Library Card"; 40]);
    let smithy = scenario
        .add_creature_from_oracle(P0, "Thousand Moons Smithy", 1, 1, THOUSAND_MOONS_SMITHY)
        .as_artifact()
        .id();
    let p1_payments: Vec<_> = (0..5)
        .map(|_| scenario.add_creature(P1, "P1 Payment", 1, 1).id())
        .collect();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Triggering Player Probe", true, "Draw a card.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    let mut payment = runner.state().objects[&smithy]
        .base_trigger_definitions
        .iter()
        .find_map(|trigger| {
            let ability = trigger.execute.as_deref()?;
            matches!(ability.effect.as_ref(), Effect::PayCost { .. }).then(|| ability.clone())
        })
        .expect("Smithy's exact Oracle must provide its first-main PayCost trigger");
    let Effect::PayCost { payer, .. } = payment.effect.as_mut() else {
        panic!("Smithy's parsed first-main trigger must begin with PayCost");
    };
    *payer = TargetFilter::TriggeringPlayer;
    let trigger = TriggerDefinition::new(TriggerMode::SpellCast).execute(payment);
    let smithy_object = runner.state_mut().objects.get_mut(&smithy).unwrap();
    smithy_object.base_trigger_definitions = Arc::new(vec![trigger]);
    smithy_object.materialize_base_trigger_definitions();
    reindex_object_triggers(runner.state_mut(), smithy);
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    runner.cast(spell).commit();
    runner.act(GameAction::PassPriority).unwrap();
    runner.act(GameAction::PassPriority).unwrap();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P1, .. }
    ));
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .unwrap();
    assert_tap_five_prompt(&runner, P1);
    let WaitingFor::PayCost { choices, .. } = &runner.state().waiting_for else {
        unreachable!();
    };
    assert!(p1_payments.iter().all(|id| choices.contains(id)));
    assert!(!choices.contains(&smithy));
}
