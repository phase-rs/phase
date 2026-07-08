//! Issue #4245: Intruder Alarm must untap creatures when a creature token enters.
//!
//! Shorikai's activated ability creates a Pilot creature token. Intruder Alarm's
//! "Whenever a creature enters, untap all creatures" must fire and untap tapped
//! creatures (Shorikai itself is a Vehicle and is not affected).

use engine::game::casting::activated_ability_definitions;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const INTRUDER_ALARM: &str =
    "Creatures don't untap during their controllers' untap steps.\nWhenever a creature enters, untap all creatures.";

const SHORIKAI: &str = "{1}, {T}: Draw two cards, then discard a card. Create a 1/1 colorless Pilot creature token with \"This token crews Vehicles as though its power were 2 greater.\"";

fn colorless_mana(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn resolve_stack(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..40 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.resolve_top();
    }
}

#[test]
fn intruder_alarm_untaps_creatures_when_pilot_token_enters_from_shorikai() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Library A", "Library B", "Library C", "Library D"]);
    let _alarm = scenario
        .add_creature(P0, "Intruder Alarm", 0, 0)
        .as_enchantment()
        .from_oracle_text(INTRUDER_ALARM)
        .id();
    let shorikai = scenario
        .add_creature(P0, "Shorikai, Genesis Engine", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Vehicle"])
        .from_oracle_text(SHORIKAI)
        .id();
    let tapped_bear = scenario.add_creature(P0, "Tapped Bear", 2, 2).id();
    let _hand_card = scenario
        .add_creature_to_hand(P0, "Discard Fodder", 1, 1)
        .id();
    scenario.with_mana_pool(P0, colorless_mana(1));

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.objects.get_mut(&tapped_bear).unwrap().tapped = true;
    }

    let ability_index = activated_ability_definitions(runner.state(), shorikai)
        .into_iter()
        .next()
        .expect("Shorikai activated ability")
        .0;
    runner.activate(shorikai, ability_index).resolve();

    // Discard prompt for Shorikai's ability.
    if let engine::types::game_state::WaitingFor::DiscardChoice { .. } = runner.state().waiting_for
    {
        let hand_card = runner.state().players[P0.0 as usize].hand[0];
        runner
            .act(GameAction::SelectCards {
                cards: vec![hand_card],
            })
            .expect("discard for Shorikai");
    }

    resolve_stack(&mut runner);

    assert!(
        !runner.state().objects[&tapped_bear].tapped,
        "Intruder Alarm must untap creatures when the Pilot token enters"
    );
    assert!(
        runner.state().objects[&shorikai].tapped,
        "Shorikai is a Vehicle, not a creature, and must remain tapped"
    );
}

#[test]
fn intruder_alarm_trigger_is_changes_zone_untap_all() {
    let mut scenario = GameScenario::new();
    let alarm = scenario
        .add_creature(P0, "Intruder Alarm", 0, 0)
        .as_enchantment()
        .from_oracle_text(INTRUDER_ALARM)
        .id();
    let runner = scenario.build();

    let trigger = &runner.state().objects[&alarm].trigger_definitions[0];
    match &*trigger.execute.as_ref().expect("execute").effect {
        Effect::SetTapState {
            scope: engine::types::ability::EffectScope::All,
            state: engine::types::ability::TapStateChange::Untap,
            ..
        } => {}
        other => panic!("expected untap-all trigger, got {other:?}"),
    }
}
