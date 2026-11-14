//! Issue #7884 — immediate `BecomesTarget` event-object bindings.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{Effect, FilterProp, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KING_OF_THE_OATHBREAKERS: &str = "Flying\n\
Whenever King of the Oathbreakers or another Spirit you control becomes the target of a spell, it phases out. (Treat it and anything attached to it as though they don't exist until your next turn.)\n\
Whenever King of the Oathbreakers or another Spirit you control phases in, create a 1/1 white Spirit creature token with flying.";

const PAWPATCH_RECRUIT: &str = "Offspring {2} (You may pay an additional {2} as you cast this spell. If you do, when this creature enters, create a 1/1 token copy of it.)\n\
Trample\n\
Whenever a creature you control becomes the target of a spell or ability an opponent controls, put a +1/+1 counter on target creature you control other than that creature.";

fn give_p1_priority(runner: &mut engine::game::scenario::GameRunner) {
    let state = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.waiting_for = WaitingFor::Priority { player: P1 };
}

#[test]
fn king_phases_out_before_broken_wings_resolves_and_the_spell_fizzles() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let king = scenario
        .add_creature_from_oracle(
            P0,
            "King of the Oathbreakers",
            3,
            3,
            KING_OF_THE_OATHBREAKERS,
        )
        .with_subtypes(vec!["Spirit"])
        .id();
    let other_spirit = scenario
        .add_creature_from_oracle(P0, "Other Spirit", 2, 2, "Flying")
        .with_subtypes(vec!["Spirit"])
        .id();
    let broken_wings = scenario
        .add_spell_to_hand_from_oracle(
            P1,
            "Broken Wings",
            true,
            "Destroy target artifact, enchantment, or creature with flying.",
        )
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    give_p1_priority(&mut runner);
    let _outcome = runner
        .cast(broken_wings)
        .target_object(other_spirit)
        .resolve();

    let king_object = &runner.state().objects[&king];
    let other_spirit_object = &runner.state().objects[&other_spirit];
    assert!(
        !king_object.is_phased_out(),
        "King is the trigger source, but only the other Spirit that became a target may phase out"
    );
    assert!(
        other_spirit_object.is_phased_out(),
        "the other Spirit that became the event target must phase out before Broken Wings resolves"
    );
    assert_eq!(
        other_spirit_object.zone,
        Zone::Battlefield,
        "a phased-out permanent remains on the battlefield"
    );
    assert_eq!(
        runner.state().objects[&broken_wings].zone,
        Zone::Graveyard,
        "Broken Wings must fizzle after its only target phases out",
    );
}

#[test]
fn pawpatch_target_prompt_excludes_the_triggering_creature_and_counters_another_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let pawpatch = scenario
        .add_creature(P0, "Pawpatch Recruit", 2, 2)
        .with_subtypes(vec!["Rabbit"])
        .from_oracle_text_with_keywords(&["Offspring", "Trample"], PAWPATCH_RECRUIT)
        .id();
    let a = scenario.add_creature(P0, "Targeted A", 2, 4).id();
    let b = scenario.add_creature(P0, "Chosen B", 2, 2).id();
    let bolt = scenario.add_bolt_to_hand(P1);

    let mut runner = scenario.build();
    give_p1_priority(&mut runner);
    let pawpatch_trigger = runner.state().objects[&pawpatch]
        .trigger_definitions
        .as_slice()
        .iter()
        .find(|entry| {
            matches!(
                entry.definition.mode,
                engine::types::triggers::TriggerMode::BecomesTarget
            )
        })
        .expect("Pawpatch BecomesTarget trigger must be present");
    let execute = pawpatch_trigger
        .definition
        .execute
        .as_deref()
        .expect("Pawpatch trigger must have an effect");
    let Effect::PutCounter { target, .. } = execute.effect.as_ref() else {
        panic!(
            "Pawpatch trigger must put a counter, got {:?}",
            execute.effect
        );
    };
    let TargetFilter::Typed(typed) = target else {
        panic!("Pawpatch trigger must have a typed creature target, got {target:?}");
    };
    assert!(
        typed.properties.iter().any(|property| {
            matches!(
                property,
                FilterProp::DistinctFrom { reference }
                    if **reference == TargetFilter::EventTarget
            )
        }),
        "Pawpatch target must exclude the event-target creature before the cast pipeline runs",
    );
    let card_id = runner.state().objects[&bolt].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Lightning Bolt must enter the normal target-selection pipeline");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(a)),
        })
        .expect("Lightning Bolt must target creature A");
    runner.advance_until_stack_empty();

    let WaitingFor::TriggerTargetSelection { selection, .. } = &runner.state().waiting_for else {
        panic!(
            "Pawpatch's triggered ability must reach the production target prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        !selection
            .current_legal_targets
            .contains(&TargetRef::Object(a)),
        "the triggering creature A must be rejected by Pawpatch's target prompt"
    );
    assert!(
        selection
            .current_legal_targets
            .contains(&TargetRef::Object(b)),
        "another controlled creature B must be accepted by Pawpatch's target prompt"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(b)),
        })
        .expect("the distinct creature B must be a legal target");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&b]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "the chosen distinct creature B must receive Pawpatch's counter",
    );
    assert_eq!(
        runner.state().objects[&a]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "the triggering creature A must not receive Pawpatch's counter",
    );
}
