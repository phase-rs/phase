//! Mazemind Tome — existential-form counter-threshold state trigger.
//!
//! Oracle (verbatim, Scryfall):
//!   "{T}, Put a page counter on this artifact: Scry 1. (Look at the top card of
//!    your library. You may put that card on the bottom.)
//!    {2}, {T}, Put a page counter on this artifact: Draw a card.
//!    When there are four or more page counters on this artifact, exile it. If
//!    you do, you gain 4 life."
//!
//! The third line is an EXISTENTIAL-there state trigger ("there are N counters
//! on ~") — a surface form the parser previously did not recognize, so the whole
//! trigger degraded to Unimplemented and never fired. This test drives the real
//! state-trigger pipeline (`check_state_triggers` → stack resolution) and asserts
//! the threshold behavior:
//!   - at 4 page counters the trigger fires: the Tome is exiled and its
//!     controller gains 4 life;
//!   - at 3 page counters it does NOT fire (the 3-vs-4 boundary is the
//!     non-vacuous discriminator).
//!
//! Reverting the `parse_source_counters_exist` combinator (or its `alt` arm in
//! `oracle_trigger`) makes the trigger Unimplemented again, so the StateCondition
//! reach-guard (and both positive assertions) flip.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 603.8: state triggers fire as soon as the game state matches the
//!     condition.
//!   - CR 122.1: a counter is a marker placed on an object.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

const MAZEMIND_TOME_ORACLE: &str = "{T}, Put a page counter on this artifact: Scry 1. (Look at the top card of your library. You may put that card on the bottom.)\n{2}, {T}, Put a page counter on this artifact: Draw a card.\nWhen there are four or more page counters on this artifact, exile it. If you do, you gain 4 life.";

/// Strip the Creature scaffolding `add_creature_from_oracle` installs and mark
/// the permanent an Artifact (Mazemind Tome's real type), clearing P/T so the
/// 0/0 body cannot die to CR 704.5f before the state trigger is checked.
fn make_artifact(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types
        .core_types
        .retain(|t| *t != CoreType::Creature);
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
    obj.base_card_types = obj.card_types.clone();
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player present")
        .life
}

/// Build a Mazemind Tome on the battlefield with `pages` page counters and run
/// the real state-trigger scan + stack drain. Returns the runner, the Tome id,
/// and the controller's life at the moment before the scan.
fn run_with_pages(pages: u32) -> (GameRunner, ObjectId, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);
    let tome = scenario
        .add_creature_from_oracle(P0, "Mazemind Tome", 0, 0, MAZEMIND_TOME_ORACLE)
        .id();
    scenario.with_counter(tome, CounterType::Generic("page".to_string()), pages);
    let mut runner = scenario.build();
    make_artifact(&mut runner, tome);

    // POSITIVE reach-guard: the third Oracle line parsed to a StateCondition
    // trigger (not Unimplemented). If the existential combinator is reverted this
    // is empty and the whole test is honestly red — no negative assertion below
    // can pass vacuously.
    assert!(
        runner.state().objects[&tome]
            .trigger_definitions
            .iter_unchecked()
            .any(|t| t.mode == TriggerMode::StateCondition),
        "Mazemind Tome must parse an existential counter-threshold StateCondition \
         trigger; got {:?}",
        runner.state().objects[&tome]
            .trigger_definitions
            .iter_unchecked()
            .map(|t| t.mode.clone())
            .collect::<Vec<_>>()
    );

    let life_before = life(&runner, P0);

    // CR 603.8 + CR 117.1: state triggers are checked whenever a player would
    // receive priority. Run the engine's state-trigger scan (the same call the
    // priority pipeline makes) and drain the stack.
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    engine::game::triggers::check_state_triggers(runner.state_mut());
    runner.advance_until_stack_empty();
    (runner, tome, life_before)
}

/// POSITIVE: at 4 page counters the existential state trigger fires — the Tome is
/// exiled and its controller gains 4 life (CR 603.8).
#[test]
fn mazemind_tome_fires_at_four_page_counters() {
    let (runner, tome, life_before) = run_with_pages(4);
    assert_eq!(
        runner.state().objects[&tome].zone,
        Zone::Exile,
        "the state trigger must exile Mazemind Tome at 4 page counters"
    );
    assert_eq!(
        life(&runner, P0) - life_before,
        4,
        "the controller must gain 4 life when the Tome is exiled"
    );
}

/// NEGATIVE discriminator: at 3 page counters the threshold is not met, so the
/// trigger does NOT fire — the Tome stays on the battlefield and life is
/// unchanged. This is the non-vacuous 3-vs-4 boundary (the StateCondition
/// reach-guard in `run_with_pages` proves the trigger is present-but-not-met, not
/// merely absent).
#[test]
fn mazemind_tome_does_not_fire_at_three_page_counters() {
    let (runner, tome, life_before) = run_with_pages(3);
    assert_eq!(
        runner.state().objects[&tome].zone,
        Zone::Battlefield,
        "below the 4-counter threshold the Tome must remain on the battlefield"
    );
    assert_eq!(
        life(&runner, P0) - life_before,
        0,
        "no life is gained below the threshold"
    );
}
