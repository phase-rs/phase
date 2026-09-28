//! A-Sigil of Myrkul's guarded reflexive combat trigger.
//!
//! CR 603.12: after the parent trigger mills, its "When you do" rider is a
//! separately created reflexive trigger. CR 603.4 checks that rider's
//! intervening-if graveyard threshold when the trigger would be created;
//! CR 608.2a checks it again when the separate trigger resolves.

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::types::ability::TargetRef;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;

use super::rules::{GameAction, GameRunner, GameScenario, Phase, WaitingFor, Zone, P0, P1};

const A_SIGIL_OF_MYRKUL_ORACLE: &str = "At the beginning of combat on your turn, mill a card. When you do, if there are four or more creature cards in your graveyard, put a +1/+1 counter on target creature you control and it gains deathtouch until end of turn.";

fn p1p1_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|object| object.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn has_deathtouch(runner: &mut GameRunner, id: ObjectId) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], &Keyword::Deathtouch)
}

/// The parent trigger only mills. Its rider has no target until its own
/// reflexive-trigger stack entry is created after that mill resolves.
fn assert_parent_has_no_rider_target(runner: &GameRunner, sigil: ObjectId) {
    let parent = runner
        .state()
        .stack
        .back()
        .expect("begin-combat trigger must be on the stack");
    assert_eq!(
        parent.source_id, sigil,
        "the pending stack entry must be A-Sigil's parent combat trigger"
    );
    assert!(
        parent
            .ability()
            .expect("trigger stack entry has a resolved ability")
            .targets
            .is_empty(),
        "CR 603.12: the parent mill trigger must not choose the rider's target before it resolves"
    );
}

/// Advance through the parent mill until its separately-created reflexive
/// trigger offers its target-selection prompt.
fn advance_to_reflexive_target_selection(runner: &mut GameRunner) {
    runner.advance_until_stack_empty();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "after a successful mill and satisfied guard, the separate reflexive trigger must ask for a target; got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn a_sigil_mills_then_creates_a_guarded_reflexive_target_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sigil = scenario
        .add_enchantment_from_oracle(P0, "A-Sigil of Myrkul", A_SIGIL_OF_MYRKUL_ORACLE)
        .id();
    let chosen = scenario.add_creature(P0, "Chosen Myrkulite", 2, 2).id();
    let friendly_decoy = scenario.add_creature(P0, "Friendly Decoy", 2, 2).id();
    let opposing_decoy = scenario.add_creature(P1, "Opponent Decoy", 2, 2).id();

    // Three creatures already in P0's graveyard plus this milled creature meet
    // the threshold only after the parent trigger's instruction has happened.
    for i in 0..3 {
        scenario.add_creature_to_graveyard(P0, &format!("P0 Graveyard Creature {i}"), 1, 1);
    }
    let milled_creature = scenario
        .add_spell_to_library_top(P0, "Milled Creature", false)
        .as_creature()
        .id();

    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);
    assert_parent_has_no_rider_target(&runner, sigil);

    advance_to_reflexive_target_selection(&mut runner);
    assert_eq!(
        runner.state().objects[&milled_creature].zone,
        Zone::Graveyard,
        "the parent trigger must mill before its reflexive rider chooses a target"
    );

    let WaitingFor::TriggerTargetSelection {
        target_slots,
        selection,
        ..
    } = runner.state().waiting_for.clone()
    else {
        unreachable!("advance_to_reflexive_target_selection checked the waiting state");
    };
    let legal_targets = &target_slots[selection.current_slot].legal_targets;
    assert!(
        legal_targets.contains(&TargetRef::Object(chosen))
            && legal_targets.contains(&TargetRef::Object(friendly_decoy)),
        "both controlled creatures must be legal reflexive targets"
    );
    assert!(
        !legal_targets.contains(&TargetRef::Object(opposing_decoy)),
        "the rider must not offer a creature controlled by the opponent"
    );
    assert!(
        legal_targets.iter().all(|target| matches!(
            target,
            TargetRef::Object(id) if runner.state().objects[id].controller == P0
        )),
        "every target offered by the reflexive trigger must be controlled by A-Sigil's controller"
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(chosen)),
        })
        .expect("the selected controlled creature must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(
        p1p1_counters(&runner, chosen),
        1,
        "the selected creature must receive exactly one +1/+1 counter"
    );
    assert!(
        has_deathtouch(&mut runner, chosen),
        "the selected creature must gain deathtouch until end of turn"
    );
    assert_eq!(
        p1p1_counters(&runner, friendly_decoy),
        0,
        "only the selected creature receives the rider's counter"
    );
}

#[test]
fn a_sigil_guard_counts_only_its_controllers_graveyard_after_milling() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sigil = scenario
        .add_enchantment_from_oracle(P0, "A-Sigil of Myrkul", A_SIGIL_OF_MYRKUL_ORACLE)
        .id();
    let friendly_creature = scenario.add_creature(P0, "Unchanged Myrkulite", 2, 2).id();

    // P0 has only two creature cards after the mill, while P1 has four. The
    // condition's Controller scope must not accidentally count P1's graveyard.
    scenario.add_creature_to_graveyard(P0, "P0 Graveyard Creature", 1, 1);
    let milled_creature = scenario
        .add_spell_to_library_top(P0, "Second Milled Creature", false)
        .as_creature()
        .id();
    for i in 0..4 {
        scenario.add_creature_to_graveyard(P1, &format!("P1 Graveyard Creature {i}"), 1, 1);
    }

    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);
    assert_parent_has_no_rider_target(&runner, sigil);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&milled_creature].zone,
        Zone::Graveyard,
        "the parent mill proves the parsed trigger reached resolution"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "with fewer than four creature cards in P0's graveyard, the rider must not ask for a target"
    );
    assert_eq!(
        p1p1_counters(&runner, friendly_creature),
        0,
        "P1's four graveyard creatures must not satisfy P0's rider condition"
    );
    assert!(
        !has_deathtouch(&mut runner, friendly_creature),
        "the failed guard must grant neither a counter nor deathtouch"
    );
}
