//! Vigor — damage prevention applies only to other creatures you control, and
//! the +1/+1 counter rider uses the prevented event's recipient.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{ShieldKind, TargetFilter};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
const VIGOR_ORACLE: &str = "Trample\n\
If damage would be dealt to another creature you control, prevent that damage. \
Put a +1/+1 counter on that creature for each 1 damage prevented this way.";

#[test]
fn vigor_does_not_prevent_damage_to_opponents_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Vigor", 6, 6, VIGOR_ORACLE);
    let goblin = scenario.add_creature(P1, "Goblin", 1, 1).id();
    let bolt = scenario.add_bolt_to_hand(P1);
    scenario.with_mana_pool(
        P1,
        vec![ManaUnit::new(
            ManaType::Red,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    // Vigor (P0) only protects P0's other creatures, so P1's Goblin takes the
    // full 3 damage and gets no counters.
    let outcome = runner.cast(bolt).target_object(goblin).resolve();

    assert_eq!(
        outcome.damage_marked(goblin),
        3,
        "damage to an opponent's creature must not be prevented by Vigor"
    );
    outcome.assert_counters(goblin, CounterType::Plus1Plus1, 0);
}

#[test]
fn vigor_prevents_damage_and_puts_counters_on_your_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let vigor = scenario
        .add_creature_from_oracle(P0, "Vigor", 6, 6, VIGOR_ORACLE)
        .id();
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let bolt = scenario.add_bolt_to_hand(P1);
    scenario.with_mana_pool(
        P1,
        vec![ManaUnit::new(
            ManaType::Red,
            engine::types::identifiers::ObjectId(0),
            false,
            vec![],
        )],
    );

    let mut runner = scenario.build();
    let vigor_repl = runner
        .state()
        .objects
        .get(&vigor)
        .expect("Vigor must exist")
        .replacement_definitions
        .iter_unchecked()
        .find(|r| r.event == ReplacementEvent::DamageDone)
        .expect("Vigor should carry a damage prevention replacement");
    assert!(matches!(
        vigor_repl.shield_kind,
        ShieldKind::Prevention { .. }
    ));
    if let TargetFilter::Typed(tf) = vigor_repl.valid_card.as_ref().expect("scoped recipient") {
        assert!(tf
            .type_filters
            .contains(&engine::types::ability::TypeFilter::Creature));
        assert!(tf
            .properties
            .contains(&engine::types::ability::FilterProp::Another));
        assert_eq!(
            tf.controller,
            Some(engine::types::ability::ControllerRef::You)
        );
    } else {
        panic!("expected typed valid_card on Vigor's prevention replacement");
    }

    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    let outcome = runner.cast(bolt).target_object(bear).resolve();

    assert_eq!(
        outcome.damage_marked(bear),
        0,
        "damage to your other creature must be fully prevented"
    );
    // one +1/+1 counter per 1 damage prevented (CR 615.5)
    outcome.assert_counters(bear, CounterType::Plus1Plus1, 3);
    // Vigor must not receive counters from protecting another creature.
    outcome.assert_counters(vigor, CounterType::Plus1Plus1, 0);
}
