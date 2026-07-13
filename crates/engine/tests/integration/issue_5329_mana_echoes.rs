//! Regression for issue #5329: Mana Echoes must add colorless mana equal to the
//! number of creatures you control sharing a creature type with the entering
//! creature — not zero due to a broken SharesQuality reference binding.
//!
//! https://github.com/phase-rs/phase/issues/5329

use engine::game::scenario::{GameScenario, P0};
use engine::game::triggers::{drain_order_triggers_with_identity, process_triggers};
use engine::game::zones::{create_object, move_to_zone};
use engine::types::ability::{FilterProp, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::CardId;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MANA_ECHOES_ORACLE: &str =
    "Whenever a creature enters, you may add an amount of {C} equal to the number of creatures you control that share a creature type with it.";

fn colorless_in_pool(runner: &engine::game::scenario::GameRunner) -> usize {
    runner.state().players[P0.0 as usize]
        .mana_pool
        .count_color(ManaType::Colorless)
}

fn enter_creature_from_hand(
    runner: &mut engine::game::scenario::GameRunner,
    name: &str,
    subtypes: &[&str],
) -> engine::types::identifiers::ObjectId {
    let state = runner.state_mut();
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, P0, name.to_string(), Zone::Hand);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(1);
    obj.toughness = Some(1);
    obj.base_power = Some(1);
    obj.base_toughness = Some(1);
    for st in subtypes {
        obj.card_types.subtypes.push((*st).to_string());
    }
    obj.base_card_types = obj.card_types.clone();
    id
}

fn resolve_mana_echoes_trigger(
    runner: &mut engine::game::scenario::GameRunner,
    mana_echoes: engine::types::identifiers::ObjectId,
    creature: engine::types::identifiers::ObjectId,
) {
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), creature, Zone::Battlefield, &mut events);
    process_triggers(runner.state_mut(), &events);
    drain_order_triggers_with_identity(runner.state_mut());

    let triggers_on_stack = runner
        .state()
        .stack
        .iter()
        .filter(|e| e.source_id == mana_echoes)
        .count();
    assert_eq!(
        triggers_on_stack,
        1,
        "Mana Echoes must trigger once on creature ETB; stack={:?}, waiting_for={:?}",
        runner.state().stack,
        runner.state().waiting_for
    );

    let mut saw_optional = false;
    for _ in 0..48 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            saw_optional = true;
            break;
        }
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to Mana Echoes trigger");
    }
    assert!(
        saw_optional,
        "Mana Echoes optional mana trigger must prompt before resolving; waiting_for={:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalEffect { accept: true })
        .expect("accept optional mana");
}

#[test]
fn mana_echoes_parse_binds_shares_type_to_triggering_source() {
    let mut scenario = GameScenario::new();
    let mana_echoes = scenario
        .add_creature(P0, "Mana Echoes", 0, 0)
        .as_enchantment()
        .from_oracle_text(MANA_ECHOES_ORACLE)
        .id();
    let runner = scenario.build();
    let trigger = &runner.state().objects[&mana_echoes].trigger_definitions[0];
    let execute = trigger.execute.as_ref().expect("execute");
    let engine::types::ability::Effect::Mana {
        produced: engine::types::ability::ManaProduction::Colorless { count },
        ..
    } = execute.effect.as_ref()
    else {
        panic!("expected colorless mana effect, got {:?}", execute.effect);
    };
    let engine::types::ability::QuantityExpr::Ref {
        qty: engine::types::ability::QuantityRef::ObjectCount { filter },
    } = count
    else {
        panic!("expected ObjectCount, got {count:?}");
    };
    let TargetFilter::Typed(tf) = filter else {
        panic!("expected typed filter, got {filter:?}");
    };
    let shares = tf.properties.iter().find_map(|p| match p {
        FilterProp::SharesQuality { reference, .. } => reference.as_deref(),
        _ => None,
    });
    assert_eq!(
        shares,
        Some(&TargetFilter::TriggeringSource),
        "shares-type reference must bind to the entering creature"
    );
}

#[test]
fn mana_echoes_adds_colorless_for_creatures_sharing_type_with_entering_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mana_echoes = scenario
        .add_creature_from_oracle(P0, "Mana Echoes", 0, 0, MANA_ECHOES_ORACLE)
        .id();
    scenario
        .add_creature(P0, "Goblin A", 1, 1)
        .with_subtypes(vec!["Goblin"]);
    scenario
        .add_creature(P0, "Goblin B", 1, 1)
        .with_subtypes(vec!["Goblin"]);

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".to_string()];
    runner.advance_until_stack_empty();

    let entering = enter_creature_from_hand(&mut runner, "Goblin C", &["Goblin"]);
    resolve_mana_echoes_trigger(&mut runner, mana_echoes, entering);

    assert_eq!(
        colorless_in_pool(&runner),
        3,
        "three Goblins share a creature type with the entering Goblin — expect 3 colorless"
    );
}

#[test]
fn mana_echoes_adds_zero_when_entering_creature_has_no_creature_subtypes() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mana_echoes = scenario
        .add_creature_from_oracle(P0, "Mana Echoes", 0, 0, MANA_ECHOES_ORACLE)
        .id();
    scenario
        .add_creature(P0, "Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"]);

    let mut runner = scenario.build();
    runner.state_mut().all_creature_types = vec!["Goblin".to_string(), "Bear".to_string()];
    runner.advance_until_stack_empty();

    // A creature with no creature subtypes shares no type with anything on board.
    let entering = enter_creature_from_hand(&mut runner, "Subtypeless", &[]);
    resolve_mana_echoes_trigger(&mut runner, mana_echoes, entering);

    assert_eq!(
        colorless_in_pool(&runner),
        0,
        "a subtypeless entering creature shares no creature type with the Goblin"
    );
}
