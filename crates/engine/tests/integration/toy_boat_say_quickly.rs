//! Toy Boat — "Cumulative upkeep—Say "Toy Boat" quickly."
//!
//! The card asks its controller to say "Toy Boat" once for each age counter on
//! it, each repetition landing inside a second ("without pausing between or
//! fumbling it"), and to sacrifice it otherwise.
//!
//! The saying is the ordinary Un-card spoken payment: `AbilityCost::SayPhrase`
//! carried by a CR 118.12a unless-cost. Two parameters carry the card text:
//! `repetitions` (raised by CR 702.24a's `PerCounter` expansion, one per age
//! counter) and `pace` (`PhrasePace::Quickly { max_gap_ms }`, the engine's
//! published window). The prompt asks for that many clicks; a lapsed window is
//! the player declining to pay, so the sacrifice comes from the engine's
//! ordinary decline path.
//!
//! These tests drive the real pipeline: the printed Oracle text is parsed into
//! `Keyword::CumulativeUpkeep`, `synthesize_cumulative_upkeep` installs the
//! upkeep trigger, and advancing to the upkeep resolves it so the age counter is
//! added and the unless-payment prompt surfaces.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::un_actions::PhrasePace;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);

/// Verbatim Oracle text from Scryfall.
const TOY_BOAT_ORACLE: &str = "Cumulative upkeep\u{2014}Say \"Toy Boat\" quickly. (At the beginning of your upkeep, put an age counter on this creature, then sacrifice it unless you say \"Toy Boat\" once for each age counter on it\u{2014}without pausing between or fumbling it.)";

/// Build Toy Boat on P0's battlefield with `preloaded_age` age counters, then
/// advance to the controller's upkeep and resolve the cumulative-upkeep trigger
/// so the engine is paused at the `UnlessPayment` prompt.
///
/// `opted_in` controls whether `P0` enabled Un-card actions; a player who has
/// not is never asked to say anything (the "unless" branch simply happens).
fn setup_at_unless_prompt(preloaded_age: u32, opted_in: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    // Start at Untap so `advance_to_upkeep` ticks into Upkeep and the
    // synthesized trigger reaches the stack (CR 503.1a).
    scenario.at_phase(Phase::Untap);

    let toy_boat = scenario
        .add_creature_from_oracle(P0, "Toy Boat", 3, 3, TOY_BOAT_ORACLE)
        .with_mana_cost(ManaCost::generic(3))
        .id();

    if preloaded_age > 0 {
        scenario.with_counter(toy_boat, CounterType::Age, preloaded_age);
    }

    let mut runner = scenario.build();
    if opted_in {
        runner.state_mut().un_actions.set_enabled(P0, true);
    }
    runner.advance_to_upkeep();
    runner.resolve_top();

    (runner, toy_boat)
}

/// CR 702.24a + Un-card (no CR entry): the resolved upkeep tick places one age
/// counter (1 -> 2) and the per-counter expansion asks for one saying of "Toy
/// Boat" per age counter, inside the "quickly" window the engine publishes.
#[test]
fn toy_boat_demands_one_saying_per_age_counter() {
    let (runner, toy_boat) = setup_at_unless_prompt(1, true);

    assert_eq!(
        runner.state().objects[&toy_boat]
            .counters
            .get(&CounterType::Age)
            .copied(),
        Some(2),
        "the upkeep tick adds one age counter"
    );

    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => {
            assert_eq!(*player, P0, "the controller is the payer");
            match cost {
                AbilityCost::SayPhrase {
                    phrase,
                    pace,
                    repetitions,
                } => {
                    assert_eq!(phrase, "Toy Boat");
                    assert_eq!(
                        pace,
                        &PhrasePace::quickly(),
                        "\"quickly\" is part of the payment, not the phrase"
                    );
                    assert_eq!(
                        pace.max_gap_ms(),
                        Some(1000),
                        "the published window is the printed one second"
                    );
                    assert_eq!(
                        *repetitions, 2,
                        "one saying per age counter (two counters after the tick)"
                    );
                }
                other => panic!("expected a SayPhrase unless-cost, got {other:?}"),
            }
        }
        other => panic!("expected the say-it unless prompt, got {other:?}"),
    }
}

/// CR 702.24a + CR 118.12a: paying says the phrase once per age counter and the
/// permanent stays on the battlefield.
#[test]
fn saying_it_once_per_counter_keeps_toy_boat() {
    let (mut runner, toy_boat) = setup_at_unless_prompt(1, true);

    let result = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("saying the phrase is accepted");

    let said: Vec<&GameEvent> = result
        .events
        .iter()
        .filter(|event| matches!(event, GameEvent::PhraseSaid { .. }))
        .collect();
    assert_eq!(
        said.len(),
        2,
        "one announcement per required saying: {:?}",
        result.events
    );
    assert!(
        said.iter().all(|event| matches!(
            event,
            GameEvent::PhraseSaid { player, phrase }
                if *player == P0 && phrase == "Toy Boat"
        )),
        "every saying is P0 saying \"Toy Boat\": {:?}",
        result.events
    );
    assert_eq!(
        runner.state().objects[&toy_boat].zone,
        Zone::Battlefield,
        "paying the cumulative-upkeep say-it cost keeps Toy Boat"
    );
}

/// CR 118.12a: a lapsed window (or any refusal) means the cost was not paid, so
/// the cumulative-upkeep sacrifice happens.
#[test]
fn pausing_or_fumbling_sacrifices_toy_boat() {
    let (mut runner, toy_boat) = setup_at_unless_prompt(1, true);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the say-it prompt must be pending before declining it"
    );

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining the unless cost is accepted");

    assert_eq!(
        runner.state().objects[&toy_boat].zone,
        Zone::Graveyard,
        "not saying it sacrifices Toy Boat"
    );
}

/// Un-card opt-in gate + CR 118.12a: a player who never enabled Un-card actions
/// is not asked to say anything, so the "unless" branch happens.
#[test]
fn an_opted_out_player_is_never_prompted_and_sacrifices() {
    let (runner, toy_boat) = setup_at_unless_prompt(1, false);

    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "no say-it prompt without opting in, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().objects[&toy_boat].zone,
        Zone::Graveyard,
        "the unprompted player sacrifices Toy Boat"
    );
}
