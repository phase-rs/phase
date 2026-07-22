//! Regression for issue #5332: Gandalf the White only parsed artifact ETB as
//! its trigger-doubling cause, dropping the legendary supertype and
//! leaves-the-battlefield half of its Oracle text.
//!
//! https://github.com/phase-rs/phase/issues/5332

use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle_static::parse_static_line;
use engine::types::ability::{StaticDefinition, TargetFilter, TriggerDefinition};
use engine::types::card_type::{CoreType, Supertype};
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, ZoneChangeRecord};
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::statics::{StaticMode, TriggerCause, ZoneChangeQualifier};
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const GANDALF_DOUBLER: &str = "If a legendary permanent or an artifact entering or leaving the battlefield causes a triggered ability of a permanent you control to trigger, that ability triggers an additional time.";

fn main_phase_two_player() -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.build().state().clone()
}

fn install_etb_observer(state: &mut GameState) -> engine::types::identifiers::ObjectId {
    let observer = create_object(
        state,
        CardId(53320),
        P0,
        "ETB Observer".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&observer).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.trigger_definitions.push(
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::Any)
            .destination(Zone::Battlefield),
    );
    observer
}

fn install_gandalf_doubler(state: &mut GameState) -> engine::types::identifiers::ObjectId {
    let StaticDefinition { mode, .. } =
        parse_static_line(GANDALF_DOUBLER).expect("Gandalf doubler static must parse");
    assert_eq!(
        mode,
        StaticMode::DoubleTriggers {
            cause: TriggerCause::BattlefieldTransition {
                enter: true,
                leave: true,
                qualifiers: vec![
                    ZoneChangeQualifier::Supertype(Supertype::Legendary),
                    ZoneChangeQualifier::CoreType(CoreType::Artifact),
                ],
            }
        },
        "Gandalf must parse legendary-or-artifact enter/leave doubling (#5332)"
    );
    let gandalf = create_object(
        state,
        CardId(53321),
        P0,
        "Gandalf the White".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&gandalf).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.static_definitions
        .push(parse_static_line(GANDALF_DOUBLER).unwrap());
    gandalf
}

#[test]
fn gandalf_parsed_static_doubles_legendary_reentry_triggers() {
    let mut state = main_phase_two_player();
    let observer = install_etb_observer(&mut state);
    let _gandalf = install_gandalf_doubler(&mut state);

    let norin = create_object(
        &mut state,
        CardId(53322),
        P0,
        "Norin the Wary".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&norin)
        .unwrap()
        .card_types
        .supertypes
        .push(Supertype::Legendary);
    state
        .objects
        .get_mut(&norin)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    let event = GameEvent::ZoneChanged {
        object_id: norin,
        from: Some(Zone::Exile),
        to: Zone::Battlefield,
        record: Box::new(ZoneChangeRecord {
            object_id: norin,
            name: "Norin the Wary".to_string(),
            core_types: vec![CoreType::Creature],
            supertypes: vec![Supertype::Legendary],
            subtypes: Vec::new(),
            keywords: Vec::new(),
            trigger_source_context: None,
            power: None,
            toughness: None,
            base_power: None,
            base_toughness: None,
            colors: Vec::new(),
            mana_value: 0,
            controller: P0,
            owner: P0,
            from_zone: Some(Zone::Exile),
            cast_from_zone: None,
            played_from_zone: None,
            to_zone: Zone::Battlefield,
            attachments: Vec::new(),
            linked_exile_snapshot: Vec::new(),
            is_token: false,
            combat_status: Default::default(),
            trigger_definitions: Vec::new(),
            co_departed: Vec::new(),
            attached_to: None,
            entered_incarnation: None,
            turn_zone_change_index: 0,
            is_suspected: false,
        }),
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
        "legendary permanent re-entering must double controlled ETB triggers under Gandalf"
    );
}
