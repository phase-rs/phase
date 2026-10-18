//! CR733 P2 coverage for the two modifier-installation families.
//!
//! Both families install a modifier that outlives the effect that created it,
//! and both wrote their mutation raw before this change, so a retained-prefix
//! replay had no record that the modifier was ever created.
//!
//! They are deliberately TWO command variants rather than one parameterized
//! variant, because the parameterization axis would straddle two CR sections the
//! engine resolves through entirely separate machinery:
//!
//! - A delayed triggered ability is CR 603.7. It never touches the CR 613 layer
//!   system; it waits in `delayed_triggers` until its condition occurs, then goes
//!   on the stack as an ordinary triggered ability (CR 603.7b). It draws NO
//!   allocator value.
//! - A transient continuous effect is CR 611.2a. It never uses the stack; it
//!   applies continuously through the CR 613 layers until its duration ends. It
//!   draws TWO allocator values — an effect id, and a CR 613.7b timestamp that
//!   orders it within its layer.
//!
//! Their `expected_*`/`resulting_*` shapes therefore differ in kind, not in a
//! leaf value: one command has an allocator receipt to verify and the other has
//! nothing to verify. Collapsing them would put two unrelated invariants behind
//! one validator arm and one applier, which is the "categorical boundary"
//! failure CLAUDE.md warns about, not the sibling-cluster smell it warns about.
//!
//! Both tests drive the REAL pipeline: a verbatim-Oracle spell cast from hand and
//! resolved off the stack, never a direct call to the authority.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::DelayedTriggerCondition;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::resolved_commands::ResolvedContinuousEffectEdit;
use engine::types::resolved_commands::{
    ResolvedContinuousEffectReplayInvariantError, ResolvedDelayedTriggerReplayInvariantError,
    ResolvedRulesCommand,
};

/// Verbatim Scryfall Oracle text. A paraphrase could take a different parser
/// branch and pass while the real card stays unjournaled.
const DISTORTION_STRIKE_ORACLE: &str = "Target creature gets +1/+0 until end of turn and can't be blocked this turn.\nRebound (If you cast this spell from your hand, exile it as it resolves. At the beginning of your next upkeep, you may cast this card from exile without paying its mana cost.)";

/// Verbatim Scryfall Oracle text.
const GIANT_GROWTH_ORACLE: &str = "Target creature gets +3/+3 until end of turn.";

/// CR 603.7 + CR 702.88a: a Rebound spell cast from hand arms a delayed
/// triggered ability that fires at its controller's next upkeep. The install
/// goes through `triggers::install_delayed_trigger`, the single authority, so it
/// lands in the journal as an exact resolved command.
#[test]
fn rebound_journals_an_exact_resolved_delayed_trigger_install() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario.add_vanilla(P0, 2, 2);
    // The MTGJSON keyword hint is what lets the keyword-only "Rebound (...)"
    // line be recognized as `Keyword::Rebound` rather than prose.
    let spell = scenario
        .add_spell_to_hand(P0, "Distortion Strike", true)
        .from_oracle_text_with_keywords(&["Rebound"], DISTORTION_STRIKE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    // Captured before the cast so the recorded command replays against the exact
    // predecessor state it was resolved from.
    let pre_state = runner.state().clone();
    let journal_start = pre_state.resolved_rules_journal.entries().len();
    assert!(
        pre_state.delayed_triggers.is_empty(),
        "the fixture must start with no delayed triggers, or the recorded install \
         position would not be the one this cast produced"
    );

    let outcome = runner.cast(spell).target_object(creature).resolve();
    let state = outcome.state();

    // REACH GUARD. Without this the journal assertion below could pass vacuously
    // on a cast whose Rebound never armed (e.g. a parser branch that dropped the
    // keyword, or a cast the engine treated as not-from-hand).
    assert_eq!(
        state.delayed_triggers.len(),
        1,
        "CR 702.88a: resolving Distortion Strike from hand must arm exactly one \
         delayed triggered ability"
    );
    assert!(
        matches!(
            state.delayed_triggers[0].condition,
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::Upkeep,
                player: P0,
                ..
            }
        ),
        "CR 702.88a: the armed trigger fires at its controller's next upkeep, found {:?}",
        state.delayed_triggers[0].condition
    );

    // DISCRIMINATING ASSERTION: a raw `delayed_triggers.push` records nothing here.
    let installs: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::DelayedTriggerInstall(command) => Some(*command),
            _ => None,
        })
        .collect();
    assert_eq!(
        installs.len(),
        1,
        "the delayed-trigger authority must journal exactly one resolved install"
    );

    let install = &installs[0];
    assert_eq!(
        install.trigger, state.delayed_triggers[0],
        "the journaled trigger is the trigger that was actually installed, with its \
         CR 603.7c bound ability intact"
    );
    assert_eq!(
        install.expected_installed_count, 0,
        "the recorded precondition is the install position observed at resolve time"
    );

    // REPLAY EXACTNESS: installing the recorded command into the captured
    // predecessor reproduces the same trigger, with no re-derivation of its
    // condition, controller, source, or bound targets.
    let mut replay = pre_state;
    engine::game::triggers::apply_resolved_delayed_trigger(&mut replay, install)
        .expect("the recorded install must replay against its captured predecessor");
    assert_eq!(
        replay.delayed_triggers, state.delayed_triggers,
        "replay installs the exact recorded delayed trigger"
    );

    // FAIL-CLOSED: the same command against a state that already has the trigger
    // is a diverged replay, and the applier must refuse rather than install a
    // duplicate CR 603.7 ability.
    assert_eq!(
        engine::game::triggers::apply_resolved_delayed_trigger(&mut replay, install),
        Err(
            ResolvedDelayedTriggerReplayInvariantError::InstalledCountPreconditionMismatch {
                expected: 0,
                found: 1,
            }
        ),
        "the applier must reject an install whose recorded position no longer matches \
         live state"
    );
    assert_eq!(
        replay.delayed_triggers.len(),
        1,
        "a rejected install must leave the collection untouched"
    );

    // A one-shot leaves the live queue once it fires, but its installation is
    // still a durable journal root. Replaying it after consumption must not
    // reuse its token/instance merely because the live queue is empty.
    let mut consumed = state.clone();
    consumed.delayed_triggers.clear();
    assert_eq!(
        engine::game::triggers::apply_resolved_delayed_trigger(&mut consumed, install),
        Err(
            ResolvedDelayedTriggerReplayInvariantError::DuplicateProvenanceToken {
                token: install.token,
            }
        ),
        "a consumed delayed trigger must retain its journaled install identity"
    );
    assert!(
        consumed.delayed_triggers.is_empty(),
        "a duplicate historical replay must not reinstall a consumed one-shot"
    );
}

/// CR 611.2a + CR 613.7b: a "gets +3/+3 until end of turn" spell creates a
/// transient continuous effect that draws an effect id and a layer timestamp.
/// `GameState::add_transient_continuous_effect` is the single authority, and it
/// journals both allocator draws so replay installs them instead of re-drawing.
#[test]
fn pump_spell_journals_an_exact_resolved_continuous_effect_install() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let creature = scenario.add_vanilla(P0, 2, 2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Giant Growth", true, GIANT_GROWTH_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    let pre_state = runner.state().clone();
    let journal_start = pre_state.resolved_rules_journal.entries().len();
    assert!(
        pre_state.transient_continuous_effects.is_empty(),
        "the fixture must start with no continuous effects, or the recorded install \
         position would not be the one this cast produced"
    );

    let outcome = runner.cast(spell).target_object(creature).resolve();
    let state = outcome.state();

    // REACH GUARD. A pump that never resolved, or one the parser routed to a
    // different effect, would leave the creature at 2/2 and make the journal
    // assertion below vacuous.
    assert_eq!(
        (
            state.objects[&creature].power,
            state.objects[&creature].toughness
        ),
        (Some(5), Some(5)),
        "CR 613.1: the resolved +3/+3 must apply to the 2/2 through the layer system"
    );
    assert_eq!(
        state.transient_continuous_effects.len(),
        1,
        "CR 611.2a: the resolved pump must install exactly one continuous effect"
    );

    // DISCRIMINATING ASSERTION: a raw `push_back` records nothing here.
    let installs: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::ContinuousEffect(command) => match *command {
                ResolvedContinuousEffectEdit::Install(install) => Some(install),
                ResolvedContinuousEffectEdit::Retire(_) => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        installs.len(),
        1,
        "the continuous-effect authority must journal exactly one resolved install"
    );

    let install = &installs[0];
    let live = &state.transient_continuous_effects[0];
    assert_eq!(
        &install.effect, live,
        "the journaled effect is the effect that was actually installed, with its \
         CR 611.2c affected set fixed"
    );
    assert_eq!(
        install.expected_installed_count, 0,
        "the recorded precondition is the install position observed at resolve time"
    );
    // CR 613.7b: the recorded timestamp is the one the effect actually received.
    // These two assertions are what make replay reproducible rather than
    // merely plausible.
    assert!(
        install.effect.timestamp < install.resulting_next_timestamp,
        "the recorded timestamp must lie below the high-water its draw left behind"
    );
    assert!(
        install.effect.id < install.resulting_next_continuous_effect_id,
        "the recorded effect id must lie below the high-water its draw left behind"
    );

    // REPLAY EXACTNESS: the effect, its id, and its layer timestamp are installed
    // verbatim, and both allocators are advanced past them so a later live draw
    // cannot hand the same values out again.
    let mut replay = pre_state;
    replay
        .apply_resolved_continuous_effect(install)
        .expect("the recorded install must replay against its captured predecessor");
    assert_eq!(
        replay.transient_continuous_effects, state.transient_continuous_effects,
        "replay installs the exact recorded continuous effect, id and timestamp included"
    );
    assert!(
        replay.next_continuous_effect_id >= install.resulting_next_continuous_effect_id,
        "replay must advance the effect-id allocator past the installed id"
    );
    assert!(
        replay.next_timestamp >= install.resulting_next_timestamp,
        "CR 613.7b: replay must advance the timestamp allocator past the installed timestamp"
    );

    // FAIL-CLOSED on the install position.
    assert_eq!(
        replay.apply_resolved_continuous_effect(install),
        Err(
            ResolvedContinuousEffectReplayInvariantError::InstalledCountPreconditionMismatch {
                expected: 0,
                found: 1,
            }
        ),
        "the applier must reject an install whose recorded position no longer matches \
         live state"
    );

    // FAIL-CLOSED on id uniqueness: a command whose position precondition DOES
    // match but whose id is already live must still be refused, because effects
    // are addressed by id and two live effects sharing one are indistinguishable
    // to every later lookup.
    let mut collides = install.clone();
    collides.expected_installed_count = 1;
    assert_eq!(
        replay.apply_resolved_continuous_effect(&collides),
        Err(ResolvedContinuousEffectReplayInvariantError::DuplicateEffectId(install.effect.id)),
        "the applier must reject an install that would duplicate a live effect id"
    );
    assert_eq!(
        replay.transient_continuous_effects.len(),
        1,
        "a rejected install must leave the collection untouched"
    );
}

fn retirement_fixture() -> (
    engine::types::game_state::GameState,
    Vec<engine::types::game_state::TransientContinuousEffect>,
) {
    use engine::types::ability::{
        ContinuousModification, Duration, ObjectScope, StaticCondition, TargetFilter,
    };
    use engine::types::game_state::TransientContinuousEffectBindings;
    use engine::types::identifiers::ObjectIncarnationRef;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_vanilla(P0, 2, 2);
    let recipient = scenario.add_vanilla(P0, 2, 2);
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&source).unwrap().tapped = true;
    let source_ref = ObjectIncarnationRef::from_object(&runner.state().objects[&source]);
    let recipient_ref = ObjectIncarnationRef::from_object(&runner.state().objects[&recipient]);
    for _ in 0..3 {
        runner
            .state_mut()
            .add_transient_continuous_effect_with_bindings(
                source,
                P0,
                Duration::ForAsLongAs {
                    condition: StaticCondition::IsTapped {
                        scope: ObjectScope::Recipient,
                    },
                },
                TargetFilter::SpecificObject { id: recipient },
                vec![ContinuousModification::AddPower { value: 1 }],
                None,
                TransientContinuousEffectBindings {
                    affected_recipient: Some(recipient_ref),
                    duration_subject: Some(source_ref),
                    granting_object: None,
                },
            );
    }
    engine::game::layers::flush_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&recipient].power, Some(5));
    assert_eq!(runner.state().transient_continuous_effects.len(), 3);
    let state = runner.state().clone();
    let effects = state.transient_continuous_effects.iter().cloned().collect();
    (state, effects)
}

fn assert_exact_retirement_rejection(
    state: engine::types::game_state::GameState,
    effects: Vec<engine::types::game_state::TransientContinuousEffect>,
    expected: engine::types::resolved_commands::ResolvedContinuousEffectRetirementInvariantError,
) {
    use engine::types::resolved_commands::{
        ResolvedContinuousEffectEditReplayInvariantError, ResolvedContinuousEffectRetirementCommand,
    };
    let mut after = state.clone();
    let cause = state
        .resolved_rules_journal
        .entries()
        .iter()
        .find_map(|entry| entry.command.as_ref().map(|_| entry.node))
        .unwrap();
    let edit = ResolvedContinuousEffectEdit::Retire(ResolvedContinuousEffectRetirementCommand {
        effects,
        cause,
    });
    assert_eq!(
        after.apply_resolved_continuous_effect_edit(&edit),
        Err(ResolvedContinuousEffectEditReplayInvariantError::Retire(
            expected
        ))
    );
    assert_eq!(
        serde_json::to_value(&after).unwrap(),
        serde_json::to_value(&state).unwrap()
    );
    assert_eq!(after.layers_dirty, state.layers_dirty);
    assert_eq!(after.resolved_rules_journal, state.resolved_rules_journal);
    assert_eq!(
        (
            after.next_timestamp,
            after.next_continuous_effect_id,
            after.next_end_effect_group_id
        ),
        (
            state.next_timestamp,
            state.next_continuous_effect_id,
            state.next_end_effect_group_id
        )
    );
}

#[test]
fn exact_retirement_validates_the_whole_batch_before_any_mutation() {
    use engine::game::scenario::P1;
    use engine::types::ability::{ContinuousModification, Duration};
    use engine::types::game_state::LayersDirty;
    use engine::types::resolved_commands::{
        ResolvedContinuousEffectRetirementCommand,
        ResolvedContinuousEffectRetirementInvariantError as Error,
    };
    let (state, effects) = retirement_fixture();
    let cause = state
        .resolved_rules_journal
        .entries()
        .iter()
        .find_map(|entry| entry.command.as_ref().map(|_| entry.node))
        .unwrap();
    let command = ResolvedContinuousEffectRetirementCommand {
        effects: vec![effects[2].clone(), effects[0].clone()],
        cause,
    };
    let mut success = state.clone();
    let journal = success.resolved_rules_journal.clone();
    let allocators = (
        success.next_timestamp,
        success.next_continuous_effect_id,
        success.next_end_effect_group_id,
    );
    success
        .apply_resolved_continuous_effect_edit(&ResolvedContinuousEffectEdit::Retire(
            command.clone(),
        ))
        .unwrap();
    assert_eq!(
        success
            .transient_continuous_effects
            .iter()
            .collect::<Vec<_>>(),
        vec![&effects[1]]
    );
    assert_eq!(success.layers_dirty, LayersDirty::Full);
    assert_eq!(success.resolved_rules_journal, journal);
    assert_eq!(
        allocators,
        (
            success.next_timestamp,
            success.next_continuous_effect_id,
            success.next_end_effect_group_id
        )
    );
    assert_exact_retirement_rejection(
        success,
        command.effects,
        Error::MissingEffect(effects[2].id),
    );
    assert_exact_retirement_rejection(state.clone(), vec![], Error::EmptyBatch);
    assert_exact_retirement_rejection(
        state.clone(),
        vec![effects[0].clone(), effects[0].clone()],
        Error::DuplicateOperandId(effects[0].id),
    );
    let mut missing = effects[2].clone();
    missing.id += 100;
    assert_exact_retirement_rejection(
        state.clone(),
        vec![effects[0].clone(), missing.clone()],
        Error::MissingEffect(missing.id),
    );
    let mut non_state = effects[1].clone();
    non_state.duration = Duration::Permanent;
    assert_exact_retirement_rejection(
        state.clone(),
        vec![effects[0].clone(), non_state],
        Error::NotStateDuration(effects[1].id),
    );
    let mut ambiguous = state.clone();
    ambiguous
        .transient_continuous_effects
        .push_back(effects[1].clone());
    assert_exact_retirement_rejection(
        ambiguous,
        vec![effects[0].clone(), effects[1].clone()],
        Error::AmbiguousStoredId(effects[1].id),
    );
    let mut mismatches = Vec::new();
    let mut changed = effects[1].clone();
    changed.duration = Duration::ForAsLongAs {
        condition: engine::types::ability::StaticCondition::DevotionGE {
            colors: vec![],
            threshold: 0,
        },
    };
    mismatches.push(changed);
    let mut changed = effects[1].clone();
    changed.condition = Some(engine::types::ability::StaticCondition::DevotionGE {
        colors: vec![],
        threshold: 0,
    });
    mismatches.push(changed);
    let mut changed = effects[1].clone();
    changed.controller = P1;
    mismatches.push(changed);
    let mut changed = effects[1].clone();
    changed.timestamp += 1;
    mismatches.push(changed);
    let mut changed = effects[1].clone();
    changed.duration_subject = changed.affected_recipient;
    mismatches.push(changed);
    let mut changed = effects[1].clone();
    changed.duration_subject.as_mut().unwrap().incarnation += 1;
    mismatches.push(changed);
    let mut changed = effects[1].clone();
    changed.affected_recipient.as_mut().unwrap().incarnation += 1;
    mismatches.push(changed);
    let mut changed = effects[1].clone();
    changed.modifications = vec![ContinuousModification::AddPower { value: 9 }];
    mismatches.push(changed);
    let mut changed = effects[1].clone();
    changed.source_name.push_str(" altered");
    mismatches.push(changed);
    for changed in mismatches {
        assert_exact_retirement_rejection(
            state.clone(),
            vec![effects[0].clone(), changed],
            Error::EffectMismatch(effects[1].id),
        );
    }
    // CR 400.7: an ended old occurrence remains the exact removal operand;
    // it must not be rebound to the same storage id's new occurrence.
    let mut stale = state.clone();
    stale
        .objects
        .get_mut(&effects[0].duration_subject.unwrap().object_id)
        .unwrap()
        .bump_incarnation();
    assert!(stale
        .transient_continuous_effects
        .iter()
        .any(|e| e == &effects[0]));
    stale
        .apply_resolved_continuous_effect_edit(&ResolvedContinuousEffectEdit::Retire(
            ResolvedContinuousEffectRetirementCommand {
                effects: vec![effects[0].clone()],
                cause,
            },
        ))
        .unwrap();
    assert!(!stale
        .transient_continuous_effects
        .iter()
        .any(|e| e.id == effects[0].id));
    let mut replacement = state.clone();
    replacement.transient_continuous_effects[0]
        .duration_subject
        .as_mut()
        .unwrap()
        .incarnation += 1;
    assert_exact_retirement_rejection(
        replacement,
        vec![effects[0].clone()],
        Error::EffectMismatch(effects[0].id),
    );
}

#[test]
fn continuous_effect_edit_preserves_all_strict_install_checks() {
    use engine::types::game_state::EndEffectGroupId;
    use engine::types::game_state::EndEffectPermission;
    use engine::types::resolved_commands::ResolvedContinuousEffectEditReplayInvariantError;
    let (state, _) = retirement_fixture();
    let install = state
        .resolved_rules_journal
        .entries()
        .iter()
        .find_map(|entry| match entry.command.as_ref()? {
            ResolvedRulesCommand::ContinuousEffect(edit) => match edit.as_ref() {
                ResolvedContinuousEffectEdit::Install(c) => Some(c.clone()),
                _ => None,
            },
            _ => None,
        })
        .unwrap();
    let mut empty = state.clone();
    empty.transient_continuous_effects.clear();
    let mut invalid = Vec::new();
    let mut c = install.clone();
    c.expected_installed_count = 1;
    invalid.push((
        c,
        ResolvedContinuousEffectReplayInvariantError::InstalledCountPreconditionMismatch {
            expected: 1,
            found: 0,
        },
    ));
    let mut c = install.clone();
    c.resulting_next_continuous_effect_id = c.effect.id;
    invalid.push((
        c,
        ResolvedContinuousEffectReplayInvariantError::IdAboveHighWater {
            id: install.effect.id,
            high_water: install.effect.id,
        },
    ));
    let mut c = install.clone();
    c.resulting_next_timestamp = c.effect.timestamp;
    invalid.push((
        c,
        ResolvedContinuousEffectReplayInvariantError::TimestampAboveHighWater {
            timestamp: install.effect.timestamp,
            high_water: install.effect.timestamp,
        },
    ));
    let mut c = install.clone();
    c.effect.end_permission = Some(EndEffectPermission {
        group: EndEffectGroupId(4),
        cost: ManaCost::zero(),
    });
    c.resulting_next_end_effect_group_id = 4;
    invalid.push((
        c,
        ResolvedContinuousEffectReplayInvariantError::EndEffectGroupAboveHighWater {
            group: 4,
            high_water: 4,
        },
    ));
    for (command, expected) in invalid {
        for unified in [false, true] {
            let mut replay = empty.clone();
            if unified {
                assert_eq!(
                    replay.apply_resolved_continuous_effect_edit(
                        &ResolvedContinuousEffectEdit::Install(command.clone())
                    ),
                    Err(ResolvedContinuousEffectEditReplayInvariantError::Install(
                        expected.clone()
                    ))
                );
            } else {
                assert_eq!(
                    replay.apply_resolved_continuous_effect(&command),
                    Err(expected.clone())
                );
            }
            assert_eq!(
                serde_json::to_value(&replay).unwrap(),
                serde_json::to_value(&empty).unwrap()
            );
            assert_eq!(replay.layers_dirty, empty.layers_dirty);
        }
    }
    let mut duplicate = install.clone();
    duplicate.expected_installed_count = state.transient_continuous_effects.len();
    assert_eq!(
        state.clone().apply_resolved_continuous_effect_edit(
            &ResolvedContinuousEffectEdit::Install(duplicate)
        ),
        Err(ResolvedContinuousEffectEditReplayInvariantError::Install(
            ResolvedContinuousEffectReplayInvariantError::DuplicateEffectId(install.effect.id)
        ))
    );
}

#[test]
fn continuous_effect_wire_reads_strict_legacy_and_tagged_operations() {
    use engine::types::resolved_commands::{
        ResolvedContinuousEffectRetirementCommand, ResolvedRulesJournal,
    };
    let (mut state, effects) = retirement_fixture();
    let install = state
        .resolved_rules_journal
        .entries()
        .iter()
        .find_map(|entry| match entry.command.as_ref()? {
            ResolvedRulesCommand::ContinuousEffect(edit) => match edit.as_ref() {
                ResolvedContinuousEffectEdit::Install(c) => Some(c.clone()),
                _ => None,
            },
            _ => None,
        })
        .unwrap();
    let retire = ResolvedContinuousEffectRetirementCommand {
        effects: vec![effects[0].clone()],
        cause: install.cause,
    };
    for edit in [
        ResolvedContinuousEffectEdit::Install(install.clone()),
        ResolvedContinuousEffectEdit::Retire(retire.clone()),
    ] {
        let value = serde_json::to_value(&edit).unwrap();
        assert_eq!(
            serde_json::from_value::<ResolvedContinuousEffectEdit>(value).unwrap(),
            edit
        );
    }
    let mut legacy = serde_json::to_value(&install).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("resulting_next_end_effect_group_id");
    let mut legacy_install = install.clone();
    legacy_install.resulting_next_end_effect_group_id = 0;
    assert_eq!(
        serde_json::from_value::<ResolvedContinuousEffectEdit>(legacy.clone()).unwrap(),
        ResolvedContinuousEffectEdit::Install(legacy_install.clone())
    );
    let old_outer = serde_json::json!({"ContinuousEffectInstall": legacy.clone()});
    assert_eq!(
        serde_json::from_value::<ResolvedRulesCommand>(old_outer).unwrap(),
        ResolvedRulesCommand::ContinuousEffect(Box::new(ResolvedContinuousEffectEdit::Install(
            legacy_install.clone()
        )))
    );
    let mut old_journal = serde_json::to_value(&state.resolved_rules_journal).unwrap();
    for entry in old_journal["entries"].as_array_mut().unwrap() {
        if let Some(install) = entry["command"]["ContinuousEffect"].get("Install").cloned() {
            entry["command"] = serde_json::json!({"ContinuousEffectInstall": install});
        }
    }
    assert_eq!(
        serde_json::from_value::<ResolvedRulesJournal>(old_journal).unwrap(),
        state.resolved_rules_journal
    );
    let mut mixed = legacy.clone();
    mixed["Retire"] = serde_json::to_value(&retire).unwrap();
    for invalid in [
        serde_json::json!({}),
        serde_json::json!({"Unknown": retire}),
        serde_json::json!({"Install": install, "Retire": retire}),
        mixed,
        serde_json::json!({"Retire": {"cause": install.cause}}),
    ] {
        assert!(serde_json::from_value::<ResolvedContinuousEffectEdit>(invalid).is_err());
    }
    let text = format!(
        "{{\"Retire\":{},\"Retire\":{}}}",
        serde_json::to_string(&retire).unwrap(),
        serde_json::to_string(&retire).unwrap()
    );
    assert!(serde_json::from_str::<ResolvedContinuousEffectEdit>(&text).is_err());
    state
        .resolved_rules_journal
        .record_continuous_effect_retirement(retire.clone())
        .unwrap();
    let valid = serde_json::to_value(&state.resolved_rules_journal).unwrap();
    assert_eq!(
        serde_json::from_value::<ResolvedRulesJournal>(valid.clone()).unwrap(),
        state.resolved_rules_journal
    );
    let index = valid["entries"].as_array().unwrap().len() - 1;
    let mut bad_cause = valid.clone();
    bad_cause["entries"][index]["command"]["ContinuousEffect"]["Retire"]["cause"] =
        serde_json::json!({"Proposal": 999999});
    let mut empty = valid.clone();
    empty["entries"][index]["command"]["ContinuousEffect"]["Retire"]["effects"] =
        serde_json::json!([]);
    let mut duplicate = valid.clone();
    duplicate["entries"][index]["command"]["ContinuousEffect"]["Retire"]["effects"] =
        serde_json::json!([effects[0], effects[0]]);
    let mut wrong_duration = valid.clone();
    wrong_duration["entries"][index]["command"]["ContinuousEffect"]["Retire"]["effects"][0]
        ["duration"] = serde_json::to_value(engine::types::ability::Duration::Permanent).unwrap();
    let mut invalid_install = valid.clone();
    let entry = invalid_install["entries"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|e| e["command"]["ContinuousEffect"].get("Install").is_some())
        .unwrap();
    entry["command"]["ContinuousEffect"]["Install"]["resulting_next_timestamp"] =
        serde_json::json!(0);
    for invalid in [bad_cause, empty, duplicate, wrong_duration, invalid_install] {
        assert!(serde_json::from_value::<ResolvedRulesJournal>(invalid).is_err());
    }
}

#[test]
fn populated_retirement_persists_checked_but_never_enters_viewer_authority() {
    use engine::game::derived_views::{ClientGameState, ClientGameStateRef};
    use engine::game::scenario::P1;
    use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
    use engine::types::game_state::PersistedGameState;
    let (mut state, effects) = retirement_fixture();
    state
        .objects
        .get_mut(&effects[0].duration_subject.unwrap().object_id)
        .unwrap()
        .tapped = false;
    engine::game::layers::mark_layers_full(&mut state);
    engine::game::layers::flush_layers(&mut state);
    assert!(state.transient_continuous_effects.is_empty());
    let retirements: Vec<_> = state
        .resolved_rules_journal
        .entries()
        .iter()
        .filter_map(|e| match e.command.as_ref()? {
            ResolvedRulesCommand::ContinuousEffect(edit) => match edit.as_ref() {
                ResolvedContinuousEffectEdit::Retire(c) => Some(c),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(retirements.len(), 1);
    assert_eq!(retirements[0].effects, effects);
    for persisted in [
        PersistedGameState::Raw(Box::new(state.clone())),
        PersistedGameState::capture(state.clone()),
    ] {
        let bytes = serde_json::to_string(&persisted).unwrap();
        let decoded: PersistedGameState = serde_json::from_str(&bytes).unwrap();
        let restored = decoded
            .into_game_state()
            .expect("checked restore accepts complete retirement authority");
        assert_eq!(
            restored.resolved_rules_journal,
            state.resolved_rules_journal
        );
        assert_eq!(
            restored.transient_continuous_effects,
            state.transient_continuous_effects
        );
    }
    for viewer in [Some(P0), Some(P1), None] {
        let projected = match viewer {
            Some(player) => filter_state_for_viewer(&state, player),
            None => filter_state_for_unseated_viewer(&state),
        };
        assert!(projected.resolved_rules_journal.entries().is_empty());
        // Visibility filtering redacts the journal in a GameState. The client
        // wire wrapper adds the marker that rejects it as restore authority.
        let filtered = serde_json::to_value(&projected).unwrap();
        assert!(filtered.get("wire_projection").is_none());
        let wire = serde_json::to_value(ClientGameStateRef::wrap(&state, viewer)).unwrap();
        assert!(wire["state"].get("resolved_rules_journal").is_none());
        assert_eq!(wire["state"]["wire_projection"], serde_json::json!(true));
        let decoded: ClientGameState = serde_json::from_value(wire.clone())
            .expect("redacted client state remains decodable for display");
        assert!(decoded.state.resolved_rules_journal.entries().is_empty());
        assert_eq!(decoded.state.viewer_projection, viewer);
        assert!(serde_json::from_value::<PersistedGameState>(wire["state"].clone()).is_err());
    }
}
