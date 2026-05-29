//! Vigor — damage prevention applies only to other creatures you control, and
//! the +1/+1 counter rider uses the prevented event's recipient.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{ShieldKind, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
const VIGOR_ORACLE: &str = "Trample\n\
If damage would be dealt to another creature you control, prevent that damage. \
Put a +1/+1 counter on that creature for each 1 damage prevented this way.";

fn plus_one_counters(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|obj| obj.counters.get(&CounterType::Plus1Plus1))
        .copied()
        .unwrap_or(0)
}

fn cast_bolt_at_creature(
    runner: &mut engine::game::scenario::GameRunner,
    bolt_id: ObjectId,
    target: ObjectId,
) {
    let bolt_card_id = runner.state().objects[&bolt_id].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: bolt_id,
            card_id: bolt_card_id,
            targets: vec![],
        })
        .expect("cast bolt");
    if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![engine::types::ability::TargetRef::Object(target)],
            })
            .expect("select creature target");
    }
    runner.advance_until_stack_empty();
}

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

    cast_bolt_at_creature(&mut runner, bolt, goblin);

    assert_eq!(
        runner.state().objects[&goblin].damage_marked,
        3,
        "damage to an opponent's creature must not be prevented by Vigor"
    );
    assert_eq!(
        plus_one_counters(&runner, goblin),
        0,
        "Vigor must not put counters on a creature it did not protect"
    );
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

    cast_bolt_at_creature(&mut runner, bolt, bear);

    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        0,
        "damage to your other creature must be fully prevented"
    );
    assert_eq!(
        plus_one_counters(&runner, bear),
        3,
        "one +1/+1 counter per 1 damage prevented (CR 615.5)"
    );
    assert_eq!(
        plus_one_counters(&runner, vigor),
        0,
        "Vigor must not receive counters from protecting another creature"
    );
}
