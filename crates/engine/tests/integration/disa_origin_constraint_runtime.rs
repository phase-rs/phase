//! CR 603.6c -- runtime discriminating tests for Disa the Restless's
//! "put into your graveyard from anywhere other than the battlefield" trigger.
//!
//! The parser fix routes richer-than-Equals/Any graveyard origins through
//! `zone_change_clauses` as `OriginConstraint::NotEquals(Zone::Battlefield)`,
//! and clears the superseded scalar `destination`/`valid_card`/`origin` fields
//! (mirrors the ETB analog at `oracle_trigger.rs:11430-11433`). These tests
//! prove that routing at runtime: a zone change whose `from` is the battlefield
//! must NOT collect Disa's trigger, while a zone change from any other origin
//! (library) MUST. Modeled on the `change_zone::resolve` journal pattern in
//! `cr733_resolved_trigger_collection.rs`.

use engine::game::effects::change_zone;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{Effect, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::zones::{EtbTapState, Zone};

/// Disa the Restless Oracle text (exact). Trigger 1 is the class fixed here;
/// trigger 2 (Tarmogoyf token) is unrelated and out of scope.
const DISA: &str = "\
Whenever a Lhurgoyf permanent card is put into your graveyard from anywhere other than the battlefield, put it onto the battlefield.\
\n\
Whenever one or more creatures you control deal combat damage to a player, create a Tarmogoyf token.";

/// `change_zone::resolve` only needs `Effect::ChangeZone`; the hand-built
/// `ResolvedAbility` frames the moved-object event. Its source_id/controller do
/// not participate in Disa's trigger matching -- the matcher keys off the
/// moved object's record against the registered observer Disa.
fn move_to_graveyard(
    state: &mut GameState,
    object_id: ObjectId,
    from: Zone,
) -> Vec<GameEvent> {
    let ability = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(from),
            destination: Zone::Graveyard,
            target: TargetFilter::SelfRef,
            owner_library: false,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: Vec::new(),
            conditional_enter_with_counters: Vec::new(),
            face_down_profile: None,
            enters_modified_if: None,
        },
        vec![TargetRef::Object(object_id)],
        ObjectId(0),
        P0,
    );
    let mut events = Vec::new();
    change_zone::resolve(state, &ability, &mut events)
        .expect("ChangeZone to graveyard must resolve");
    events
}

/// True iff Disa's `source_id` appears among collected trigger contexts. A
/// matching ChangesZone trigger is parked in `GameState::deferred_triggers`
/// (the queue drained at priority) -- see `cr733` asserting the same.
fn disa_trigger_collected(state: &GameState, disa: ObjectId) -> bool {
    state
        .deferred_triggers
        .iter()
        .any(|ctx| ctx.pending.source_id == disa)
}

/// Positive case: a Lhurgoyf permanent card put into the graveyard from the
/// library (origin != Battlefield) MUST fire Disa's trigger.
#[test]
fn disa_trigger_fires_for_library_to_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let disa = scenario
        .add_creature_from_oracle(P0, "Disa the Restless", 2, 3, DISA)
        .id();
    // Lhurgoyf permanent card on the battlefield; with_subtypes tags it
    // "Lhurgoyf" so Disa's valid_card filter matches.
    let lurker = scenario
        .add_creature(P0, "Lhurgoyf Lurker", 2, 2)
        .with_subtypes(vec!["Lhurgoyf"])
        .id();
    // Build first (scenario.state is crate-private), then relocate the lurker
    // to the library to set the POSITIVE firing origin (Library != Battlefield).
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&lurker).unwrap();
        obj.zone = Zone::Library;
        obj.summoning_sick = true;
    }
    move_to_graveyard(runner.state_mut(), lurker, Zone::Library);
    assert_eq!(
        runner.state().objects[&lurker].zone,
        Zone::Graveyard,
        "the moved card must land in the graveyard"
    );
    assert!(
        disa_trigger_collected(runner.state(), disa),
        "Disa's trigger MUST fire when a Lhurgoyf card enters the graveyard \
         from the library (NotEquals(Battlefield) clause allows it)"
    );
}

/// Negative case: a Lhurgoyf permanent card put into the graveyard FROM the
/// battlefield must NOT fire Disa's trigger -- "from anywhere OTHER THAN the
/// battlefield" excludes `from == Battlefield`.
#[test]
fn disa_trigger_does_not_fire_for_battlefield_to_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let disa = scenario
        .add_creature_from_oracle(P0, "Disa the Restless", 2, 3, DISA)
        .id();
    let lurker = scenario
        .add_creature(P0, "Lhurgoyf Lurker", 2, 2)
        .with_subtypes(vec!["Lhurgoyf"])
        .id();
    // Lurker starts on the battlefield -- the NEGATIVE firing origin.
    let mut runner = scenario.build();
    move_to_graveyard(runner.state_mut(), lurker, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&lurker].zone,
        Zone::Graveyard,
        "the card still moves to the graveyard -- only the trigger is excluded"
    );
    assert!(
        !disa_trigger_collected(runner.state(), disa),
        "Disa's trigger must NOT fire when a Lhurgoyf card enters the graveyard \
         FROM the battlefield -- NotEquals(Battlefield) must reject it"
    );
}
