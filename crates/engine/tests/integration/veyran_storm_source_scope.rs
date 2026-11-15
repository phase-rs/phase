//! Veyran doubles triggered abilities of permanents, not spell abilities.

use engine::game::scenario::{GameScenario, P0};
use engine::types::game_state::{StackEntryKind, SyntheticTriggerProvenance};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const VEYRAN_DOUBLER_ORACLE: &str = "If you casting or copying an instant or sorcery spell causes a triggered ability of a permanent you control to trigger, that ability triggers an additional time.";
const CAST_WITNESS_ORACLE: &str = "Whenever you cast an instant or sorcery spell, add {R}.";
const CAST_THIS_SPELL_ORACLE: &str = "When you cast this spell, draw a card.";
const PROWESS_WITNESS_ORACLE: &str = "Prowess";
const SIMPLE_SPELL_ORACLE: &str = "Draw a card.";
const CHATTERSTORM_ORACLE: &str = "Convoke\n\
Create a 1/1 green Squirrel creature token.\n\
Storm (When you cast this spell, copy it for each spell cast before it this turn. You may choose new targets for the copies.)";

/// CR 603.2d + CR 702.40a: Veyran's "ability of a permanent" scope excludes
/// Storm, a triggered ability of the spell on the stack.
#[test]
fn veyran_does_not_double_storm() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Veyran, Voice of Duality", 2, 2, VEYRAN_DOUBLER_ORACLE);
    let chatterstorm = scenario
        .add_spell_to_hand_from_oracle(P0, "Chatterstorm", false, CHATTERSTORM_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let commit = runner.cast(chatterstorm).commit();
    let state = commit.state();
    let storm_triggers = state
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility {
                    provenance: Some(SyntheticTriggerProvenance::Storm { .. }),
                    ..
                }
            )
        })
        .count();

    assert_eq!(
        storm_triggers, 1,
        "Veyran must not double Storm because Storm belongs to the spell, not a permanent"
    );
}

/// CR 603.2d: Veyran doubles a cast-triggered ability of a controlled
/// battlefield permanent.
#[test]
fn veyran_doubles_cast_trigger_of_battlefield_permanent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Veyran, Voice of Duality", 2, 2, VEYRAN_DOUBLER_ORACLE);
    let witness = scenario
        .add_creature_from_oracle(P0, "Cast Witness", 1, 1, CAST_WITNESS_ORACLE)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Simple Spell", false, SIMPLE_SPELL_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let commit = runner.cast(spell).commit();
    let witness_triggers = commit
        .state()
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == witness
                && matches!(&entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .count();

    assert_eq!(
        witness_triggers, 2,
        "Veyran must double a cast-triggered ability from a controlled permanent"
    );
}

/// CR 702.108a + CR 603.2d: Keyword-synthesized triggers do not capture a
/// source context, but their live battlefield source still qualifies for
/// Veyran's permanent scope.
#[test]
fn veyran_doubles_prowess_without_captured_source_context() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Veyran, Voice of Duality", 2, 2, VEYRAN_DOUBLER_ORACLE);
    let prowess_witness = scenario
        .add_creature_from_oracle(P0, "Prowess Witness", 1, 1, PROWESS_WITNESS_ORACLE)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Simple Spell", false, SIMPLE_SPELL_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let commit = runner.cast(spell).commit();
    let prowess_triggers = commit
        .state()
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == prowess_witness
                && matches!(&entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .count();

    assert_eq!(
        prowess_triggers, 2,
        "Veyran must double Prowess while its source remains on the battlefield"
    );
}

/// CR 403.3 + CR 603.2d: A creature spell is not a permanent while it is on
/// the stack, so Veyran must not double its "when you cast this spell" trigger.
#[test]
fn veyran_does_not_double_cast_trigger_of_permanent_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Veyran, Voice of Duality", 2, 2, VEYRAN_DOUBLER_ORACLE);
    let creature_spell = scenario
        .add_creature_to_hand_from_oracle(P0, "Stack-Born Witness", 1, 1, CAST_THIS_SPELL_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let commit = runner.cast(creature_spell).commit();
    let creature_spell_triggers = commit
        .state()
        .stack
        .iter()
        .filter(|entry| {
            entry.source_id == creature_spell
                && matches!(&entry.kind, StackEntryKind::TriggeredAbility { .. })
        })
        .count();

    assert_eq!(
        creature_spell_triggers, 1,
        "Veyran must not double a trigger whose source is a creature spell on the stack"
    );
}
