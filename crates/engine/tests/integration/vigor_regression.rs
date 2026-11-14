//! Vigor — damage prevention applies only to other creatures you control, and
//! the +1/+1 counter rider uses the prevented event's recipient.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::{ShieldKind, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::replacements::ReplacementEvent;
const VIGOR_ORACLE: &str = "Trample\n\
If damage would be dealt to another creature you control, prevent that damage. \
Put a +1/+1 counter on that creature for each 1 damage prevented this way.\n\
When Vigor is put into a graveyard from anywhere, shuffle it into its owner's library.";

#[test]
fn vigor_thirty_blockers_complete_prevention_riders_and_reach_next_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest"]);
    scenario.with_library_top(P1, &["Island", "Island"]);
    let vigor = scenario
        .add_creature_from_oracle(P0, "Vigor", 6, 6, VIGOR_ORACLE)
        .with_plus_counters(2)
        .id();
    let attacker = scenario
        .add_creature(P0, "Protected attacker", 4, 4)
        .with_plus_counters(28)
        .id();
    let blockers: Vec<_> = (0..30)
        .map(|index| {
            scenario
                .add_creature(P1, &format!("Blocker {index}"), 2, 5)
                .id()
        })
        .collect();
    let mut runner = scenario.build();
    let initial_turn = runner.state().turn_number;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare protected attacker");
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner
            .act(GameAction::PassPriority)
            .expect("advance to blockers");
    }
    runner
        .declare_blockers(
            &blockers
                .iter()
                .map(|&blocker| (blocker, attacker))
                .collect::<Vec<_>>(),
        )
        .expect("all thirty creatures block the attacker");
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::AssignCombatDamage { .. }
        ) {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner
            .act(GameAction::PassPriority)
            .expect("advance to damage assignment");
    }
    let WaitingFor::AssignCombatDamage {
        attacker_id,
        blockers: slots,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected actual combat assignment, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*attacker_id, attacker);
    assert_eq!(slots.len(), 30);
    assert!(runner.state().resolution_stack.is_empty());
    let assignment = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .find(|action| matches!(action, GameAction::AssignCombatDamage { .. }))
        .expect("engine supplies legal damage assignment");
    let result = runner
        .act(assignment)
        .expect("deal the assigned combat damage");

    // CR 510.2 + CR 615.5: all thirty blockers deal simultaneously; each
    // prevention rider completes immediately using that event's recipient.
    let prevented: Vec<_> = result
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamagePrevented { amount, .. } => Some(*amount),
            _ => None,
        })
        .collect();
    assert!(
        !prevented.is_empty(),
        "combat damage was actually prevented"
    );
    assert_eq!(prevented.iter().sum::<u32>(), 60);
    let counters: Vec<_> = result
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CounterAdded {
                object_id,
                counter_type: CounterType::Plus1Plus1,
                count,
                ..
            } => Some((*object_id, *count)),
            _ => None,
        })
        .collect();
    assert_eq!(counters.len(), 30);
    assert!(counters
        .iter()
        .all(|(id, count)| *id == attacker && *count == 2));
    assert_eq!(
        runner.state().objects[&attacker].counters[&CounterType::Plus1Plus1],
        88
    );
    assert_eq!(
        runner.state().objects[&vigor].counters[&CounterType::Plus1Plus1],
        2
    );
    assert_eq!(runner.state().objects[&attacker].damage_marked, 0);
    assert!(
        runner.state().resolution_stack.is_empty(),
        "completed riders must not leave false paused drains"
    );

    for _ in 0..32 {
        if runner.state().turn_number > initial_turn {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "combat and end-step work must finish: {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::PassPriority)
            .expect("ordinary passes reach the next turn");
    }
    assert_eq!(runner.state().turn_number, initial_turn + 1);
    assert_eq!(runner.state().active_player, P1);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().resolution_stack.is_empty());
    assert_eq!(
        runner.state().objects[&attacker].counters[&CounterType::Plus1Plus1],
        88
    );
    assert_eq!(
        runner.state().objects[&vigor].counters[&CounterType::Plus1Plus1],
        2
    );
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
