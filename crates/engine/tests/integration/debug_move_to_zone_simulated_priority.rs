//! A sandbox move that simulates processing requires a settled Priority window
//! (issue #10397), mirroring the `Debug::CreateCard { run_etb: true }` guard.
//!
//! CR 616.1 + CR 603.3b: a simulated move runs the full destination pipeline
//! whatever the destination, and two tails of it can park a private prompt.
//! Entry replacements and "as enters" choices (CR 614.12) can propose one for a
//! battlefield destination, and the state-based-action and trigger dispatch
//! that follows every simulated move can park an `OrderTriggers` ordering
//! choice, a commander-zone choice, a replacement-order choice, or a legend
//! choice. Only a settled Priority boundary can own such a frame; admitting the
//! move next to an open prompt would replace the outstanding proposal before it
//! resolves, and the preempted prompt never resumes. The raw move
//! (`simulate: false`) installs nothing, so it keeps its previous behavior.
//!
//! The pending prompt is production-made: casting the real Shock (committed
//! fixture face, verbatim Oracle text "Shock deals 2 damage to any target.")
//! opens its target prompt and it stays unanswered. The move's object is the
//! real Elspeth Tirel in the graveyard, so a successful simulated entry also
//! shows the CR 306.5b intrinsic loyalty counters arriving as usual.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::EngineError;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const ELSPETH: &str = "Elspeth Tirel";
const SHOCK: &str = "Shock";
const GUARD: &str = "Debug::MoveToZone with simulated processing requires a Priority window";

/// Elspeth Tirel's printed loyalty (committed fixture face, CR 306.5b).
const ELSPETH_LOYALTY: u32 = 4;

fn db() -> &'static CardDatabase {
    shared_card_db().expect("the committed integration fixture loads")
}

/// A main-phase table with the move's object (P0's Elspeth Tirel) staged in the
/// graveyard, plus P0's Shock and a Mountain and P1's 2/2 for the prompt row.
fn staged() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, SHOCK, Zone::Hand, db());
    scenario.add_basic_land(P0, ManaColor::Red);
    scenario.add_vanilla(P1, 2, 2);
    let planeswalker = scenario.add_real_card(P0, ELSPETH, Zone::Graveyard, db());
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    (runner, planeswalker)
}

/// Cast the staged Shock through the real cast action and leave its target
/// prompt unanswered.
fn cast_shock(runner: &mut GameRunner) {
    let (object_id, card_id) = runner
        .state()
        .objects
        .values()
        .find(|object| object.name == SHOCK && object.zone == Zone::Hand)
        .map(|object| (object.id, object.card_id))
        .expect("the staged Shock is in hand");
    runner
        .act(GameAction::CastSpell {
            object_id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("the Shock cast is offered");
}

fn short(waiting_for: &WaitingFor) -> String {
    format!("{waiting_for:?}").chars().take(120).collect()
}

/// Reach guard: the cast really opened Shock's target prompt, for P0, with a
/// slot to fill. Returns the pending wait state for an unchanged comparison.
fn pending_target_prompt(runner: &GameRunner) -> WaitingFor {
    let waiting_for = runner.state().waiting_for.clone();
    match &waiting_for {
        WaitingFor::TargetSelection {
            player,
            target_slots,
            ..
        } => {
            assert_eq!(*player, P0, "reach: the Shock caster owns the prompt");
            assert!(
                !target_slots.is_empty(),
                "reach: Shock's target slot exists: {waiting_for:?}"
            );
        }
        other => panic!("reach: the cast left a target prompt, got {}", short(other)),
    }
    waiting_for
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn loyalty(runner: &GameRunner, id: ObjectId) -> Option<u32> {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Loyalty)
        .copied()
}

fn is_priority(runner: &GameRunner) -> bool {
    matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
}

/// Submit `MoveToZone { to_zone, simulate }` through the production
/// `GameAction::Debug` boundary.
fn move_to(
    runner: &mut GameRunner,
    object_id: ObjectId,
    to_zone: Zone,
    simulate: bool,
) -> Result<(), EngineError> {
    runner
        .act(GameAction::Debug(DebugAction::MoveToZone {
            object_id,
            to_zone,
            library_position: None,
            simulate,
        }))
        .map(|_| ())
}

/// The pending wait state with one engine-owned bookkeeping carrier normalized
/// away.
///
/// While a cast waits for its target, `PendingCast::activation_trigger_collection`
/// is where `process_triggers` journaled the events it prepared; an admitted
/// nested debug action therefore appends its own prepared batch to that
/// carrier's `operation_journal`. That is the journal recording the action, not
/// the prompt being replaced, so it is the only field this comparison excludes
/// — the carrier's `overlay` and `pending_contexts`, the wait variant, player,
/// pending cast, slots, and selection must all be identical.
fn without_event_journal(waiting_for: &WaitingFor) -> serde_json::Value {
    let mut value = serde_json::to_value(waiting_for).expect("the prompt serializes");
    if let Some(collection) = value
        .get_mut("data")
        .and_then(|data| data.get_mut("pending_cast"))
        .and_then(|pending_cast| pending_cast.get_mut("activation_trigger_collection"))
        .and_then(serde_json::Value::as_object_mut)
    {
        collection.remove("operation_journal");
    }
    value
}

/// Assert the pending target prompt kept every field except that journal.
fn assert_prompt_intact(runner: &GameRunner, pending: &WaitingFor, context: &str) {
    assert_eq!(
        without_event_journal(&runner.state().waiting_for),
        without_event_journal(pending),
        "{context}: the pending target prompt is intact"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "{context}: the prompt is still the target selection"
    );
}

fn assert_guard_rejection(runner: &mut GameRunner, object_id: ObjectId, to_zone: Zone) {
    let error = move_to(runner, object_id, to_zone, true)
        .expect_err("a simulated move is refused under an open prompt");
    match error {
        EngineError::InvalidAction(message) => assert_eq!(message, GUARD),
        other => panic!("expected InvalidAction({GUARD:?}), got {other:?}"),
    }
}

/// A simulated battlefield move beside a pending prompt is rejected, and the
/// rejection leaves both the prompt and the object untouched.
#[test]
fn simulated_battlefield_move_is_rejected_under_a_pending_prompt() {
    let (mut runner, planeswalker) = staged();
    cast_shock(&mut runner);
    let pending = pending_target_prompt(&runner);
    assert_eq!(
        zone_of(&runner, planeswalker),
        Zone::Graveyard,
        "the object starts in the graveyard"
    );

    assert_guard_rejection(&mut runner, planeswalker, Zone::Battlefield);

    assert_eq!(
        runner.state().waiting_for,
        pending,
        "the pending target prompt is unchanged"
    );
    assert_eq!(
        zone_of(&runner, planeswalker),
        Zone::Graveyard,
        "the rejected move leaves the object in its original zone"
    );
    assert_eq!(
        loyalty(&runner, planeswalker),
        None,
        "the rejected move puts no entry counters anywhere"
    );
}

/// With nobody prompted, the same simulated battlefield move applies through
/// the real entry pipeline and settles back at Priority.
#[test]
fn simulated_battlefield_move_from_priority_applies() {
    let (mut runner, planeswalker) = staged();
    assert!(
        is_priority(&runner),
        "reach: the settled board waits at Priority, got {}",
        short(&runner.state().waiting_for)
    );
    assert_eq!(zone_of(&runner, planeswalker), Zone::Graveyard);

    move_to(&mut runner, planeswalker, Zone::Battlefield, true)
        .expect("a Priority window admits the simulated entry");

    assert_eq!(
        zone_of(&runner, planeswalker),
        Zone::Battlefield,
        "the object entered the battlefield"
    );
    assert_eq!(
        loyalty(&runner, planeswalker),
        Some(ELSPETH_LOYALTY),
        "CR 306.5b: the printed loyalty counters arrived with the entry"
    );
    assert!(
        is_priority(&runner),
        "the entry settles back at Priority, got {}",
        short(&runner.state().waiting_for)
    );
}

/// The raw move is not the issue's scope: it consults nothing, so it stays
/// allowed and leaves the pending prompt in place.
#[test]
fn raw_battlefield_move_is_allowed_under_a_pending_prompt() {
    let (mut runner, planeswalker) = staged();
    cast_shock(&mut runner);
    let pending = pending_target_prompt(&runner);

    move_to(&mut runner, planeswalker, Zone::Battlefield, false)
        .expect("a raw battlefield move needs no Priority window");

    assert_eq!(
        zone_of(&runner, planeswalker),
        Zone::Battlefield,
        "the raw placement applied"
    );
    assert_prompt_intact(&runner, &pending, "raw move");
}

/// A simulated move to a non-battlefield destination still runs the
/// state-based-action and trigger dispatch, which can park a prompt, so it is
/// rejected beside an open prompt too: the rejection leaves both the prompt and
/// the object untouched.
#[test]
fn simulated_non_battlefield_move_is_rejected_under_a_pending_prompt() {
    let (mut runner, planeswalker) = staged();
    cast_shock(&mut runner);
    let pending = pending_target_prompt(&runner);
    assert_eq!(
        zone_of(&runner, planeswalker),
        Zone::Graveyard,
        "the object starts in the graveyard"
    );

    assert_guard_rejection(&mut runner, planeswalker, Zone::Graveyard);

    assert_prompt_intact(&runner, &pending, "simulated graveyard rejection");
    assert_eq!(
        zone_of(&runner, planeswalker),
        Zone::Graveyard,
        "the rejected move leaves the object in its original zone"
    );
    assert_eq!(
        loyalty(&runner, planeswalker),
        None,
        "the rejected move puts no entry counters anywhere"
    );
}

/// The same non-battlefield simulated move applies from a Priority window: the
/// dispatch runs with nobody preempted and the action settles back at Priority.
#[test]
fn simulated_non_battlefield_move_from_priority_applies() {
    let (mut runner, planeswalker) = staged();
    assert!(
        is_priority(&runner),
        "reach: the settled board waits at Priority, got {}",
        short(&runner.state().waiting_for)
    );
    assert_eq!(zone_of(&runner, planeswalker), Zone::Graveyard);

    move_to(&mut runner, planeswalker, Zone::Hand, true)
        .expect("a Priority window admits the simulated non-battlefield move");

    assert_eq!(
        zone_of(&runner, planeswalker),
        Zone::Hand,
        "the simulated move applied to its non-battlefield destination"
    );
    assert!(
        is_priority(&runner),
        "the move settles back at Priority, got {}",
        short(&runner.state().waiting_for)
    );
}
