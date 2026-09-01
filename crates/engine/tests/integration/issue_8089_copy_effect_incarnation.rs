//! Regression coverage for journaled one-shot continuous-effect recipients.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::ability::{
    Duration, Effect, ObjectScope, ResolvedAbility, StaticCondition, TargetFilter, TargetRef,
};
use engine::types::game_state::GameState;
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::resolved_commands::{ResolvedContinuousEffectCommand, ResolvedRulesCommand};
use engine::types::zones::Zone;

fn recorded_installs_since(
    state: &GameState,
    journal_start: usize,
) -> Vec<ResolvedContinuousEffectCommand> {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.as_ref())
        .filter_map(|command| match command {
            ResolvedRulesCommand::ContinuousEffectInstall(command) => {
                Some(command.as_ref().clone())
            }
            _ => None,
        })
        .collect()
}

fn copy_ability(
    source: ObjectId,
    recipient: TargetFilter,
    targets: Vec<TargetRef>,
) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::BecomeCopy {
            recipient,
            target: TargetFilter::Any,
            duration: Some(Duration::UntilEndOfTurn),
            mana_value_limit: None,
            additional_modifications: Vec::new(),
        },
        targets,
        source,
        P0,
    )
}

fn assert_command_pins_match_live(
    state: &GameState,
    installs: &[ResolvedContinuousEffectCommand],
    expected_recipients: &[ObjectIncarnationRef],
) {
    assert_eq!(installs.len(), expected_recipients.len());
    assert_eq!(
        state.transient_continuous_effects.len(),
        expected_recipients.len()
    );

    let mut command_pins: Vec<_> = installs
        .iter()
        .map(|install| install.effect.affected_recipient)
        .collect();
    let mut live_pins: Vec<_> = state
        .transient_continuous_effects
        .iter()
        .map(|effect| effect.affected_recipient)
        .collect();
    let mut expected_pins: Vec<_> = expected_recipients.iter().copied().map(Some).collect();
    command_pins.sort();
    live_pins.sort();
    expected_pins.sort();

    assert_eq!(command_pins, expected_pins);
    assert_eq!(live_pins, expected_pins);
    assert_eq!(command_pins, live_pins);
}

#[test]
fn self_copy_journals_its_recipient_and_cannot_copy_after_reentry() {
    let mut scenario = GameScenario::new();
    let donor = scenario.add_creature(P0, "Journal Donor", 5, 5).id();
    let source = scenario.add_creature(P0, "Journal Copy Host", 1, 1).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();
    let pre_state = state.clone();
    let journal_start = state.resolved_rules_journal.entries().len();
    let expected_recipient = ObjectIncarnationRef::from_object(&state.objects[&source]);
    let ability = copy_ability(
        source,
        TargetFilter::SelfRef,
        vec![TargetRef::Object(donor)],
    );

    engine::game::effects::become_copy::resolve(&mut state, &ability, &mut Vec::new())
        .expect("the actual SelfRef resolver must install the copy");
    let installs = recorded_installs_since(&state, journal_start);

    assert_eq!(
        state.objects[&source].name, "Journal Donor",
        "the SelfRef BecomeCopy path must reach the layer-1 copy effect"
    );
    assert_command_pins_match_live(&state, &installs, &[expected_recipient]);

    let mut replay = pre_state;
    replay
        .apply_resolved_continuous_effect(&installs[0])
        .expect("the recorded copy install must replay against its predecessor");
    evaluate_layers(&mut replay);
    assert_eq!(replay.objects[&source].name, "Journal Donor");
    assert_command_pins_match_live(&replay, &installs, &[expected_recipient]);

    let mut events = Vec::new();
    assert!(
        !move_object_for_test(
            &mut replay,
            ZoneMoveRequest::effect(source, Zone::Graveyard, donor),
            &mut events,
        ),
        "the source's graveyard move must complete without a replacement choice"
    );
    assert!(
        !move_object_for_test(
            &mut replay,
            ZoneMoveRequest::effect(source, Zone::Battlefield, donor),
            &mut events,
        ),
        "the source's return must complete without a replacement choice"
    );
    evaluate_layers(&mut replay);
    assert_eq!(
        replay.objects[&source].name, "Journal Copy Host",
        "a replayed copy effect must not apply to the source's new incarnation"
    );
}

#[test]
fn parent_target_copy_journals_a_distinct_pin_for_each_recipient() {
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Copy Source", 1, 1).id();
    let donor = scenario.add_creature(P0, "Parent Donor", 5, 5).id();
    let recipient = scenario.add_creature(P0, "Parent Recipient", 2, 2).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();
    let pre_state = state.clone();
    let journal_start = state.resolved_rules_journal.entries().len();
    let expected = [
        ObjectIncarnationRef::from_object(&state.objects[&donor]),
        ObjectIncarnationRef::from_object(&state.objects[&recipient]),
    ];
    let ability = copy_ability(
        source,
        TargetFilter::ParentTarget,
        vec![TargetRef::Object(donor), TargetRef::Object(recipient)],
    );

    engine::game::effects::become_copy::resolve(&mut state, &ability, &mut Vec::new())
        .expect("the actual ParentTarget resolver must install both copies");
    let installs = recorded_installs_since(&state, journal_start);
    assert_command_pins_match_live(&state, &installs, &expected);

    let mut replay = pre_state;
    for install in &installs {
        replay
            .apply_resolved_continuous_effect(install)
            .expect("each recorded ParentTarget install must replay in order");
    }
    assert_command_pins_match_live(&replay, &installs, &expected);
}

#[test]
fn mass_copy_journals_a_distinct_pin_for_every_resolved_recipient() {
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Mass Copy Source", 1, 1).id();
    let donor = scenario.add_creature(P0, "Mass Donor", 5, 5).id();
    let host = scenario.add_creature(P1, "Mass Host", 2, 2).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();
    let pre_state = state.clone();
    let journal_start = state.resolved_rules_journal.entries().len();
    let expected: Vec<_> = state
        .battlefield
        .iter()
        .map(|id| ObjectIncarnationRef::from_object(&state.objects[id]))
        .collect();
    let ability = copy_ability(source, TargetFilter::Any, vec![TargetRef::Object(donor)]);

    engine::game::effects::become_copy::resolve(&mut state, &ability, &mut Vec::new())
        .expect("the actual mass-copy resolver must install every copy");
    let installs = recorded_installs_since(&state, journal_start);
    assert_command_pins_match_live(&state, &installs, &expected);
    assert_eq!(state.objects[&host].name, "Mass Donor");

    let mut replay = pre_state;
    for install in &installs {
        replay
            .apply_resolved_continuous_effect(install)
            .expect("each recorded mass-copy install must replay in order");
    }
    assert_command_pins_match_live(&replay, &installs, &expected);
}

#[test]
fn force_block_journals_its_exact_recipient_pin() {
    let mut scenario = GameScenario::new();
    let source = scenario.add_creature(P0, "Force Block Source", 2, 2).id();
    let blocker = scenario
        .add_creature(P1, "Force Block Recipient", 2, 2)
        .id();
    let runner = scenario.build();
    let mut state = runner.state().clone();
    let pre_state = state.clone();
    let journal_start = state.resolved_rules_journal.entries().len();
    let expected_recipient = ObjectIncarnationRef::from_object(&state.objects[&blocker]);
    let ability = ResolvedAbility::new(
        Effect::ForceBlock {
            target: TargetFilter::Any,
            attacker: None,
            duration: Duration::UntilEndOfTurn,
        },
        vec![TargetRef::Object(blocker)],
        source,
        P0,
    );

    engine::game::effects::force_block::resolve(&mut state, &ability, &mut Vec::new())
        .expect("the actual ForceBlock resolver must install its requirement");
    let installs = recorded_installs_since(&state, journal_start);
    assert_command_pins_match_live(&state, &installs, &[expected_recipient]);

    let mut replay = pre_state;
    replay
        .apply_resolved_continuous_effect(&installs[0])
        .expect("the recorded ForceBlock install must replay against its predecessor");
    assert_eq!(
        replay.transient_continuous_effects,
        state.transient_continuous_effects
    );
}

#[test]
fn zygon_duration_subject_replay_cannot_follow_a_reentered_tapped_target() {
    let mut scenario = GameScenario::new();
    let target = scenario.add_creature(P0, "Tapped Target", 5, 5).id();
    let zygon = scenario.add_creature(P0, "Zygon Infiltrator", 2, 3).id();
    let runner = scenario.build();
    let mut state = runner.state().clone();
    state.objects.get_mut(&target).unwrap().tapped = true;
    let pre_state = state.clone();
    let journal_start = state.resolved_rules_journal.entries().len();
    let target_ref = ObjectIncarnationRef::from_object(&state.objects[&target]);

    let ability = ResolvedAbility::new(
        Effect::BecomeCopy {
            recipient: TargetFilter::SelfRef,
            target: TargetFilter::Any,
            duration: Some(Duration::ForAsLongAs {
                condition: StaticCondition::IsTapped {
                    scope: ObjectScope::Target,
                },
            }),
            mana_value_limit: None,
            additional_modifications: Vec::new(),
        },
        vec![TargetRef::Object(target)],
        zygon,
        P0,
    );

    engine::game::effects::become_copy::resolve(&mut state, &ability, &mut Vec::new())
        .expect("Zygon's copied target must install its duration-bound copy effect");
    let installs = recorded_installs_since(&state, journal_start);
    assert_eq!(installs.len(), 1);
    assert_eq!(installs[0].effect.duration_subject, Some(target_ref));

    let mut replay = pre_state;
    replay
        .apply_resolved_continuous_effect(&installs[0])
        .expect("the recorded Zygon copy install must replay against its predecessor");
    evaluate_layers(&mut replay);
    assert_eq!(replay.objects[&zygon].name, "Tapped Target");

    replay.objects.get_mut(&target).unwrap().tapped = false;
    evaluate_layers(&mut replay);
    assert_eq!(
        replay.objects[&zygon].name, "Zygon Infiltrator",
        "untapping the captured target must end the copy"
    );

    let mut events = Vec::new();
    assert!(
        !move_object_for_test(
            &mut replay,
            ZoneMoveRequest::effect(target, Zone::Graveyard, zygon),
            &mut events,
        ),
        "the target's graveyard move must complete without a replacement choice"
    );
    assert!(
        !move_object_for_test(
            &mut replay,
            ZoneMoveRequest::effect(target, Zone::Battlefield, zygon),
            &mut events,
        ),
        "the target's return must complete without a replacement choice"
    );
    replay.objects.get_mut(&target).unwrap().tapped = true;
    evaluate_layers(&mut replay);
    assert_eq!(
        replay.objects[&zygon].name, "Zygon Infiltrator",
        "a reentered tapped object cannot revive a duration bound to its old incarnation"
    );
}
