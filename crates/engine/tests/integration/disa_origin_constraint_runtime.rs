//! CR 603.6c -- runtime discriminating tests for Disa the Restless's
//! "put into your graveyard from anywhere other than the battlefield" trigger.
//!
//! The parser fix routes richer-than-Equals/Any graveyard origins through
//! `zone_change_clauses` as `OriginConstraint::NotEquals(Zone::Battlefield)`,
//! and clears the superseded scalar `destination`/`valid_card`/`origin` fields
//! (mirrors the ETB analog at `oracle_trigger.rs:11430-11433`). These tests
//! prove that routing at runtime: a zone change whose `from` is the battlefield
//! must NOT collect Disa's trigger, while a zone change from any other origin
//! (library) MUST. Modeled on the `change_zone::resolve` + journal pattern in
//! `cr733_resolved_trigger_collection.rs`.

use engine::game::effects::change_zone;
use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::move_to_zone;
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

/// `change_zone::resolve` hand-builds a `ChangeZone` effect framing the moved
/// object event. The `source_id` MUST be a real registered object (Disa here)
/// so the resolver can consult controller/ability provenance for framing; the
/// trigger matcher itself keys off the moved object's `record.object_id`
/// against the registered observer, not off this source. `origin: None` lets
/// the resolver derive the event's `from` zone from the moved object's actual
/// current location (which is what Disa's `NotEquals(Battlefield)` clause
/// reads), and `target: Any` with a pre-selected `TargetRef::Object` resolves
/// to that chosen target (mirrors `cr733`).
fn move_to_graveyard(
    state: &mut GameState,
    source_id: ObjectId,
    object_id: ObjectId,
) -> Vec<GameEvent> {
    // Mirrors cr733's proven construction exactly:
    // - `origin: None` lets the resolver derive the event's `from` zone from the
    //   moved object's actual current zone (Library for the +case, Battlefield
    //   for the -case) -- which is precisely the clause value Disa's
    //   `NotEquals(Battlefield)` matcher reads.
    // - `target: TargetFilter::Any` with a pre-selected `TargetRef::Object`
    //   resolves to that chosen target (cr733 does the same). Do NOT use
    //   `SelfRef`: it short-circuits to the ability `source_id` (Disa) and
    //   would move the wrong object / hit the origin-mismatch no-op guard.
    let ability = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: None,
            destination: Zone::Graveyard,
            target: TargetFilter::Any,
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
        source_id,
        P0,
    );
    let mut events = Vec::new();
    change_zone::resolve(state, &ability, &mut events)
        .expect("ChangeZone to graveyard must resolve");
    events
}

/// True iff Disa's `source_id` appears among collected trigger contexts. A
/// matching ChangesZone trigger is parked in `GameState::deferred_triggers`
/// (the queue drained at priority) -- see `cr733` asserting the same field.
fn disa_trigger_collected(state: &GameState, disa: ObjectId) -> bool {
    state
        .deferred_triggers
        .iter()
        .any(|ctx| ctx.pending.source_id == disa)
}

/// Shared scaffold: Disa (P0, with her parsed Oracle text -> her ChangesZone
/// trigger is registered as an observable) plus a Lhurgoyf permanent card
/// (subtype "Lhurgoyf", owned by P0) so her `valid_card` filter matches.
fn scenario_with_disa_and_lurker() -> (engine::game::scenario::GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let disa = scenario
        .add_creature_from_oracle(P0, "Disa the Restless", 2, 3, DISA)
        .id();
    let lurker = scenario
        .add_creature(P0, "Lhurgoyf Lurker", 2, 2)
        .with_subtypes(vec!["Lhurgoyf"])
        .id();
    (scenario.build(), disa, lurker)
}

/// Positive case: a Lhurgoyf permanent card put into the graveyard from the
/// library (origin != Battlefield) MUST fire Disa's trigger.
#[test]
fn disa_trigger_fires_for_library_to_graveyard() {
    let (mut runner, disa, lurker) = scenario_with_disa_and_lurker();
    // Relocate the lurker from the battlefield to the library via the engine's
    // index-correct `move_to_zone` (field-poking `obj.zone` would desync the
    // player library/battlefield indices). `from == Library` is the POSITIVE,
    // non-excluded firing origin.
    move_to_zone(runner.state_mut(), lurker, Zone::Library, &mut Vec::new());
    assert_eq!(
        runner.state().objects[&lurker].zone,
        Zone::Library,
        "lurker must start the zone change in the library"
    );
    move_to_graveyard(runner.state_mut(), disa, lurker);
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
    let (mut runner, disa, lurker) = scenario_with_disa_and_lurker();
    // Lurker starts on the battlefield -- the NEGATIVE, excluded firing origin.
    assert_eq!(
        runner.state().objects[&lurker].zone,
        Zone::Battlefield,
        "lurker must start on the battlefield for the negative case"
    );
    move_to_graveyard(runner.state_mut(), disa, lurker);
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
