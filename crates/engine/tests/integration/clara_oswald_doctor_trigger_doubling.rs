//! Cluster-110 Seam #2 regression: Clara Oswald's "If a triggered ability of a
//! Doctor you control triggers, that ability triggers an additional time." must
//! double triggers ONLY from Doctors you control (CR 603.2d), honoring the
//! Doctor-scoped `affected` filter on the enclosing `StaticDefinition` — never
//! every controlled trigger. This locks the parser output (`DoubleTriggers`
//! with a `Some(affected)` filter) against a regression to the pre-fix
//! `affected: null` shape that would over-double.

use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::create_object;
use engine::parser::oracle_static::parse_static_line;
use engine::types::ability::{TargetFilter, TriggerDefinition};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::statics::StaticMode;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const CLARA_DOUBLER: &str =
    "If a triggered ability of a Doctor you control triggers, that ability triggers an additional time.";

fn main_phase() -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.build().state().clone()
}

/// Install Clara Oswald's Doctor-scoped trigger doubler on a battlefield object.
fn install_clara_doubler(state: &mut GameState) {
    let def = parse_static_line(CLARA_DOUBLER).expect("Clara doubler static must parse");
    assert!(
        matches!(def.mode, StaticMode::DoubleTriggers { .. }),
        "Clara must parse as a DoubleTriggers static, got {:?}",
        def.mode
    );
    assert!(
        def.affected.is_some(),
        "Clara's doubler must carry a Doctor-scoped `affected` filter (not null); got {:?}",
        def.affected
    );

    let clara = create_object(
        state,
        CardId(1100),
        P0,
        "Clara Oswald".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&clara).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.static_definitions.push(def);
}

/// A creature you control with a `DamageDone` trigger observer, optionally a Doctor.
fn install_observer(
    state: &mut GameState,
    id: u64,
    name: &str,
    is_doctor: bool,
) -> engine::types::identifiers::ObjectId {
    let observer = create_object(state, CardId(id), P0, name.to_string(), Zone::Battlefield);
    let obj = state.objects.get_mut(&observer).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    if is_doctor {
        obj.card_types.subtypes.push("Doctor".to_string());
    }
    obj.trigger_definitions
        .push(TriggerDefinition::new(TriggerMode::DamageDone).valid_card(TargetFilter::Any));
    observer
}

fn count_triggers_from(state: &GameState, source: engine::types::identifiers::ObjectId) -> usize {
    state.stack.iter().filter(|e| e.source_id == source).count()
}

#[test]
fn clara_doubles_only_doctor_controlled_triggers() {
    let mut state = main_phase();
    install_clara_doubler(&mut state);
    let doctor = install_observer(&mut state, 1101, "The Doctor", true);
    let non_doctor = install_observer(&mut state, 1102, "Ordinary Bear", false);

    // Some external damage event both observers react to (source is an opponent
    // permanent so neither observer is the damage source — only the reaction
    // trigger matters).
    let source = create_object(
        &mut state,
        CardId(1103),
        engine::types::player::PlayerId(1),
        "Opponent Source".to_string(),
        Zone::Battlefield,
    );
    let event = GameEvent::DamageDealt {
        source_id: source,
        target: engine::types::ability::TargetRef::Object(source),
        amount: 1,
        is_combat: false,
        excess: 0,
    };

    engine::game::triggers::process_triggers(&mut state, &[event]);
    engine::game::triggers::drain_order_triggers_with_identity(&mut state);

    // CR 603.2d: the Doctor's trigger is doubled (present twice); the non-Doctor's
    // is not (present once) — proving the doubling honors the Doctor `affected`
    // filter rather than doubling every controlled trigger.
    assert_eq!(
        count_triggers_from(&state, doctor),
        2,
        "a Doctor you control's trigger must be doubled by Clara"
    );
    assert_eq!(
        count_triggers_from(&state, non_doctor),
        1,
        "a non-Doctor's trigger must NOT be doubled (Doctor-scoped affected filter)"
    );
}
