//! Issue #5247 (p0-panic) — Kinetic Ooze: "double the number of +1/+1 counters
//! on any number of other target creatures" dropped its variable-target bound.
//!
//! The `MultiplyCounter` clause bound a single REQUIRED creature target instead
//! of a `MultiTargetSpec` of "any number" (min 0), so resolving the X>=10 ETB
//! sub-clause raised `Invalid action: Unused selected target slots` and broke the
//! game. Restoring the multi-target bound lets the controller pick any number of
//! other creatures and doubles their +1/+1 counters cleanly.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const KINETIC_OOZE: &str = "This creature enters with X +1/+1 counters on it.\n\
When this creature enters, destroy up to one target artifact or enchantment with mana value X or less. \
If X is 5 or more, you draw a card. \
If X is 10 or more, double the number of +1/+1 counters on any number of other target creatures.";

#[test]
fn kinetic_ooze_x10_doubles_counters_on_other_target_creature_without_panic() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The first optional target must be consumed before the conditional
    // counter-doubling target is selected. This is the reported failing shape:
    // before the regression fix, reserving the conditional target caused the
    // first selection to fail with "Unused selected target slots".
    let artifact = scenario
        .add_creature(P1, "Clockwork Relic", 1, 1)
        .as_artifact()
        .id();
    // The "other target creature" whose +1/+1 counters the X>=10 clause doubles.
    let bear = scenario.add_creature(P0, "Grizzly Bear", 2, 2).id();
    scenario.with_counter(bear, CounterType::Plus1Plus1, 1);
    let recipient = scenario.add_creature(P0, "Balduvian Bears", 2, 2).id();
    scenario.with_counter(recipient, CounterType::Plus1Plus1, 1);
    // Remains legal after the declared bear leaves the battlefield. The trigger
    // must not replace its announced target with this creature at resolution.
    let decoy = scenario.add_creature(P0, "Runeclaw Bear", 2, 2).id();
    scenario.with_counter(decoy, CounterType::Plus1Plus1, 1);
    // The X>=5 draw clause needs a card in the library to avoid an empty-library
    // draw game-loss.
    scenario.add_card_to_library_top(P0, "Library Card");

    let ooze = scenario
        .add_spell_to_hand(P0, "Kinetic Ooze", false)
        .from_oracle_text(KINETIC_OOZE)
        .with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![],
        })
        .id();

    // Ten generic mana to pay {X} with X = 10.
    scenario.with_mana_pool(
        P0,
        (0..10)
            .map(|i| ManaUnit::new(ManaType::Colorless, ObjectId(9_900 + i), false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&artifact].zone,
        Zone::Battlefield,
        "reach guard: the destroy instruction must receive a battlefield artifact"
    );

    // Cast with X = 10, then put the ETB trigger on the stack. Both targets
    // must be selected now: the artifact for the first instruction and the bear
    // for the ordinary X>=10 conditional instruction (CR 603.3d / CR 601.2c).
    let mut committed = runner.cast(ooze).x(10).commit();
    committed
        .act(GameAction::PassPriority)
        .expect("pass priority to resolve Kinetic Ooze");
    committed
        .act(GameAction::PassPriority)
        .expect("pass priority to put Kinetic Ooze's ETB on the stack");

    let first_target = match &committed.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            ref target_slots,
            ref selection,
            ..
        } => target_slots[selection.current_slot].legal_targets.clone(),
        ref other => panic!("expected first ETB target prompt, got {other:?}"),
    };
    assert!(
        first_target.contains(&TargetRef::Object(artifact)),
        "the destroy instruction must receive the first announced target"
    );
    committed
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(artifact)),
        })
        .expect("choose the artifact for Kinetic Ooze's destroy instruction");

    let conditional_target = match &committed.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            ref target_slots,
            ref selection,
            ..
        } => target_slots[selection.current_slot].legal_targets.clone(),
        ref other => {
            panic!("expected X>=10 target prompt while stacking the trigger, got {other:?}")
        }
    };
    assert!(
        conditional_target.contains(&TargetRef::Object(bear)),
        "the X>=10 conditional target must be announced before the trigger resolves"
    );
    committed
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(bear)),
        })
        .expect("choose the bear for Kinetic Ooze's counter-doubling instruction");

    let second_conditional_target = match &committed.state().waiting_for {
        WaitingFor::TriggerTargetSelection {
            ref target_slots,
            ref selection,
            ..
        } => target_slots[selection.current_slot].legal_targets.clone(),
        ref other => panic!("expected another any-number target prompt, got {other:?}"),
    };
    assert!(
        second_conditional_target.contains(&TargetRef::Object(recipient)),
        "the X>=10 instruction must accept every announced target in its any-number set"
    );
    committed
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(recipient)),
        })
        .expect("choose a second creature for Kinetic Ooze's counter-doubling instruction");

    while matches!(
        &committed.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        committed
            .act(GameAction::ChooseTarget { target: None })
            .expect("finish optional any-number target selections");
    }

    // The declared bear becomes illegal while the trigger waits on the stack,
    // while the decoy remains legal. Its target identity must not be replaced.
    {
        let state = committed.state_mut();
        state.battlefield.retain(|id| *id != bear);
        state.objects.get_mut(&bear).expect("bear exists").zone = Zone::Graveyard;
        state.players[P0.0 as usize].graveyard.push_back(bear);
    }

    let outcome = committed.resolve();

    // The p0 fix: on `main` this panics with `Invalid action: Unused selected
    // target slots` while resolving the ETB; with the multi-target bound restored
    // the whole cast → ETB → trigger chain resolves cleanly back to Priority.
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Kinetic Ooze's ETB must resolve cleanly (no 'Unused selected target slots' \
         panic), got {:?}",
        outcome.final_waiting_for()
    );

    // The cast/ETB pipeline ran to completion: the Ooze remains on the battlefield
    // and the trigger resolved rather than aborting mid-target-selection.
    assert!(
        runner.state().objects.contains_key(&ooze),
        "Kinetic Ooze must resolve onto the battlefield"
    );
    assert!(
        runner.state().objects[&artifact].zone == Zone::Graveyard,
        "the first optional target must be assigned to the destroy instruction"
    );
    assert!(
        runner.state().objects.contains_key(&bear),
        "the declared bear remains represented after changing zones"
    );
    assert_eq!(
        runner.state().objects[&bear].zone,
        Zone::Graveyard,
        "the declared bear left before resolution"
    );
    assert_eq!(
        runner.state().objects[&bear]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1),
        "the bear left the battlefield after being announced and must not be replaced"
    );
    assert_eq!(
        runner.state().objects[&recipient]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&2),
        "a legal announced target's +1/+1 counters must double"
    );
    assert_eq!(
        runner.state().objects[&decoy]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1),
        "a newly preferred legal creature must not replace the announced bear target"
    );
}
