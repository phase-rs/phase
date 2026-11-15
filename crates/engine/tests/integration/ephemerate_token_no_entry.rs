//! CR 111.8: a token that has left the battlefield can't come back, so the
//! return leg of a blink is no entry at all — no replacement consult, no
//! enters-with counters, no `CounterAdded`.
//!
//! Issue #10396: Ephemerate on a Spike Feeder **token** applied the token's own
//! "enters with two +1/+1 counters" replacement while the token sat in exile.
//! The ChangeZone resolvers reach `zone_pipeline::execute_zone_move_with_applied_terminal`
//! directly; that funnel seeded entry counters and consulted
//! `replacement::replace_event` without the object-level no-op guard
//! `move_object_with_terminal` runs (`zone_pipeline::zone_move_is_primitive_noop`,
//! CR 111.8 + CR 400.6). The primitive then refused the move, so the counters /
//! `ReplacementApplied` were reported for a move that never happened
//! (CR 603.6d, CR 614.7, CR 614.12).
//!
//! Rows:
//! - R1: Ephemerate on a Spike Feeder token — no counters, no `CounterAdded`,
//!   no `ReplacementApplied`, token ceases to exist (CR 704.5d).
//! - R2 (control): Ephemerate on a real Spike Feeder card — the new object
//!   enters with exactly two +1/+1 counters (CR 400.7, CR 614.1c).
//! - R3 (control): Cloudshift on a plain creature — the blink still happens.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::{create_debug_cards, debug_card_entry_source, DebugCardCreateRequest};
use engine::types::actions::DebugCardCreationKind;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const SPIKE_FEEDER: &str = "Spike Feeder";
const EPHEMERATE: &str = "Exile target creature you control, then return it to the battlefield under its owner's control.\nRebound (If you cast this spell from your hand, exile it as it resolves. At the beginning of your next upkeep, you may cast this card from exile without paying its mana cost.)";
const CLOUDSHIFT: &str =
    "Exile target creature you control, then return that card to the battlefield under your control.";
const GRIZZLY_BEARS: &str = "Grizzly Bears";

fn db() -> &'static CardDatabase {
    shared_card_db().expect("the committed integration fixture loads")
}

/// Create one `name` through the engine's debug Create Card entry.
fn create(
    runner: &mut GameRunner,
    name: &str,
    zone: Zone,
    kind: DebugCardCreationKind,
) -> ObjectId {
    let db = db();
    let face = db
        .get_face_by_name(name)
        .unwrap_or_else(|| panic!("{name} is in the committed fixture"));
    create_debug_cards(
        runner.state_mut(),
        DebugCardCreateRequest {
            actor: P0,
            source: debug_card_entry_source(db, face),
            owner: P0,
            zone,
            count: 1,
            attach_to: None,
            run_etb: true,
            nonlegendary: false,
            creation_kind: kind,
        },
    )
    .unwrap_or_else(|error| panic!("the debug creation of {name} is accepted: {error:?}"));
    only_object_named(runner, name)
}

fn only_object_named(runner: &GameRunner, name: &str) -> ObjectId {
    let ids: Vec<ObjectId> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.name == name)
        .map(|object| object.id)
        .collect();
    assert_eq!(ids.len(), 1, "exactly one {name} exists: {ids:?}");
    ids[0]
}

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn add_ephemerate(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Ephemerate", true, EPHEMERATE)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id()
}

/// A main-phase board with Ephemerate already in P0's hand.
fn board_with_ephemerate() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_ephemerate(&mut scenario);
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    (runner, spell)
}

fn cast_and_drain(runner: &mut GameRunner, spell: ObjectId, target: ObjectId) -> Vec<GameEvent> {
    let mut events = runner
        .cast(spell)
        .target_object(target)
        .resolve()
        .events()
        .to_vec();
    while !runner.state().stack.is_empty() {
        let result = runner
            .act(engine::types::actions::GameAction::PassPriority)
            .expect("priority pass must advance resolution");
        events.extend(result.events);
    }
    events
}

fn counters_put_on(events: &[GameEvent], id: ObjectId) -> u32 {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CounterAdded {
                object_id,
                counter_type,
                count,
                ..
            } if *object_id == id && *counter_type == CounterType::Plus1Plus1 => Some(*count),
            _ => None,
        })
        .sum()
}

/// R1: the token's return leg is refused, so nothing is consulted or applied.
#[test]
fn r1_ephemerate_on_a_spike_feeder_token_applies_no_entry_counters() {
    let (mut runner, ephemerate) = board_with_ephemerate();
    let token = create(
        &mut runner,
        SPIKE_FEEDER,
        Zone::Battlefield,
        DebugCardCreationKind::Token,
    );
    assert!(runner.state().objects[&token].is_token);
    assert_eq!(
        p1p1(&runner, token),
        2,
        "reach guard: the token entered with two"
    );

    let events = cast_and_drain(&mut runner, ephemerate, token);

    assert!(
        events.iter().any(
            |event| matches!(event, GameEvent::ZoneChanged { object_id, .. } if *object_id == token)
        ),
        "reach guard: the exile leg happened: {events:?}"
    );
    assert_eq!(
        counters_put_on(&events, token),
        0,
        "CR 111.8 + CR 614.7: no enters-with counters for a token that cannot return: {events:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            GameEvent::ReplacementApplied { source_id, .. } if *source_id == token
        )),
        "CR 111.8: no replacement is consulted or reported for the refused return: {events:?}"
    );
    assert!(
        !runner.state().objects.contains_key(&token),
        "CR 704.5d: the token ceased to exist"
    );
}

/// R2: a real Spike Feeder card blinks and re-enters with its counters.
#[test]
fn r2_ephemerate_on_a_real_spike_feeder_enters_with_two_counters() {
    let (mut runner, ephemerate) = board_with_ephemerate();
    let feeder = create(
        &mut runner,
        SPIKE_FEEDER,
        Zone::Battlefield,
        DebugCardCreationKind::Card,
    );
    assert!(!runner.state().objects[&feeder].is_token);
    let incarnation = runner.state().objects[&feeder].incarnation;

    let events = cast_and_drain(&mut runner, ephemerate, feeder);

    assert_eq!(
        runner.state().objects[&feeder].zone,
        Zone::Battlefield,
        "the return leg re-enters the battlefield"
    );
    assert_ne!(
        runner.state().objects[&feeder].incarnation,
        incarnation,
        "CR 400.7: the returned permanent is a new object"
    );
    assert_eq!(
        p1p1(&runner, feeder),
        2,
        "CR 614.1c: the new object enters with exactly two +1/+1 counters"
    );
    assert_eq!(
        counters_put_on(&events, feeder),
        2,
        "one CounterAdded of two, not a re-application: {events:?}"
    );
}

/// R3: the guard does not block an ordinary blink of a plain creature.
#[test]
fn r3_cloudshift_on_a_plain_creature_still_blinks() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = scenario.add_real_card(P0, GRIZZLY_BEARS, Zone::Battlefield, db());
    let cloudshift = scenario
        .add_spell_to_hand_from_oracle(P0, "Cloudshift", true, CLOUDSHIFT)
        .with_mana_cost(engine::types::mana::ManaCost::zero())
        .id();
    let mut runner = scenario.build();
    let incarnation = runner.state().objects[&bears].incarnation;

    let events = cast_and_drain(&mut runner, cloudshift, bears);

    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    assert_ne!(
        runner.state().objects[&bears].incarnation,
        incarnation,
        "CR 400.7: the blink still returns a new object"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged { object_id, to: Zone::Battlefield, .. } if *object_id == bears
        )),
        "the return leg still delivers: {events:?}"
    );
}
