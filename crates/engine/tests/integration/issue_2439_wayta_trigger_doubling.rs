//! Regression for issue #2439: Wayta, Trainer Prodigy doubles all triggered
//! abilities instead of only damage-caused ones.
//!
//! https://github.com/phase-rs/phase/issues/2439

use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle_static::parse_static_line;
use engine::types::ability::{StaticDefinition, TargetFilter, TriggerDefinition};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{StaticMode, TriggerCause};
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const WAYTA_DOUBLER: &str = "If a creature you control being dealt damage causes a triggered ability of a permanent you control to trigger, that ability triggers an additional time.";

fn main_phase_two_player() -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.build().state().clone()
}

fn install_damage_observer(state: &mut GameState) -> engine::types::identifiers::ObjectId {
    let observer = create_object(
        state,
        CardId(2400),
        P0,
        "Damage Observer".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&observer).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.trigger_definitions
        .push(TriggerDefinition::new(TriggerMode::DamageDone).valid_card(TargetFilter::Any));
    observer
}

fn install_wayta_doubler(state: &mut GameState) -> engine::types::identifiers::ObjectId {
    let StaticDefinition { mode, .. } =
        parse_static_line(WAYTA_DOUBLER).expect("Wayta doubler static must parse");
    assert_eq!(
        mode,
        StaticMode::DoubleTriggers {
            cause: TriggerCause::ControlledCreatureDealtDamage
        },
        "Wayta must parse as damage-caused trigger doubling (#2439)"
    );
    let wayta = create_object(
        state,
        CardId(2401),
        P0,
        "Wayta, Trainer Prodigy".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&wayta).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.static_definitions
        .push(parse_static_line(WAYTA_DOUBLER).unwrap());
    wayta
}

#[test]
fn wayta_parsed_static_doubles_only_damage_caused_triggers() {
    let mut state = main_phase_two_player();
    let observer = install_damage_observer(&mut state);
    let _wayta = install_wayta_doubler(&mut state);

    let damaged = create_object(
        &mut state,
        CardId(2402),
        P0,
        "Your Creature".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&damaged)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    let source = create_object(
        &mut state,
        CardId(2403),
        PlayerId(1),
        "Opponent Source".to_string(),
        Zone::Battlefield,
    );

    let event = GameEvent::DamageDealt {
        source_id: source,
        target: engine::types::ability::TargetRef::Object(damaged),
        amount: 1,
        is_combat: false,
        excess: 0,
    };

    engine::game::triggers::process_triggers(&mut state, &[event]);
    engine::game::triggers::drain_order_triggers_with_identity(&mut state);

    let doubled = state
        .stack
        .iter()
        .filter(|e| e.source_id == observer)
        .count();
    assert_eq!(
        doubled, 2,
        "damage to your creature must double the observer's damage trigger once"
    );
}
