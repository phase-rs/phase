//! Toy Boat — "Cumulative upkeep—Say \"Toy Boat\" quickly."
//!
//! The card asks its controller to say "Toy Boat" once for each age counter on
//! it, each repetition landing inside a second ("without pausing between or
//! fumbling it"), and to sacrifice it otherwise.
//!
//! The saying is the ordinary Un-card spoken payment: `AbilityCost::SayPhrase`
//! carried by a CR 118.12a unless-cost. Two parameters carry the card text:
//! `repetitions` (raised by CR 702.24a's `PerCounter` expansion, one per age
//! counter) and `pace` (`PhrasePace::Quickly`, the engine's published window).
//!
//! # Who decides the timing
//!
//! The engine does, and it never reads a clock. Exactly like the real-time
//! contest model (`types::contest`), the HOST stamps each observed saying with a
//! monotonic millisecond instant and a per-payment sequence
//! (`GameAction::SayPhrase`), the engine stores the deadline, and it validates
//! the stamp deterministically. A saying after the stored deadline is a pause,
//! which leaves the cost unpaid and produces the ordinary sacrifice. A boolean
//! `PayUnlessCost { pay: true }` cannot pay a timed cost at all: without a
//! stamped saying the engine has no evidence the phrase was said in time.
//!
//! These tests drive the real pipeline: the printed Oracle text is parsed into
//! `Keyword::CumulativeUpkeep`, `synthesize_cumulative_upkeep` installs the
//! upkeep trigger, and advancing to the upkeep resolves it so the age counter is
//! added and the unless-payment prompt surfaces with its engine-minted instance.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::ability::AbilityCost;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::un_actions::{
    PhrasePace, SayPhraseDisposition, SayPhraseObservation, SayPhrasePayment,
};
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

/// The engine-minted instance advertised by the live prompt. Every host
/// observation names it, so a delayed frame cannot be mistaken for activity in a
/// later payment.
fn payment_id(runner: &GameRunner) -> u64 {
    runner
        .state()
        .say_payment
        .as_ref()
        .expect("a timed spoken prompt carries its engine-owned instance")
        .id
}

/// The sayings still owed by the live prompt, read from its cost.
fn owed(runner: &GameRunner) -> u32 {
    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment {
            cost: AbilityCost::SayPhrase { repetitions, .. },
            ..
        } => *repetitions,
        other => panic!("expected the say-it unless prompt, got {other:?}"),
    }
}

fn say(runner: &mut GameRunner, seq: u32, at_ms: u64) -> engine::types::game_state::ActionResult {
    let payment = payment_id(runner);
    runner
        .act(GameAction::SayPhrase {
            payment,
            seq,
            at_ms,
        })
        .expect("the host-stamped saying is accepted")
}

/// The phrases announced in `events`, in order.
fn phrases_said(events: &[GameEvent]) -> Vec<(PlayerId, String)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PhraseSaid { player, phrase } => Some((*player, phrase.clone())),
            _ => None,
        })
        .collect()
}

/// CR 702.24a + Un-card (no CR entry): the resolved upkeep tick places one age
/// counter (1 -> 2), the per-counter expansion asks for one saying of "Toy Boat"
/// per age counter inside the "quickly" window, and the engine mints the payment
/// instance that owns the identity, progress and deadline.
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

    let payment = runner
        .state()
        .say_payment
        .as_ref()
        .expect("the prompt carries a payment instance");
    assert_eq!(payment.owed, 2, "the instance owns the full requirement");
    assert_eq!(payment.said, 0, "nothing has been reported yet");
    assert!(
        payment.deadline_ms.is_none(),
        "reading the prompt is never a pause: the window starts at the first saying"
    );
}

/// CR 702.24a + CR 118.12a + Un-card (no CR entry): the engine owns the count.
/// Each host observation reports exactly ONE saying and the prompt comes back
/// carrying the sayings still owed; only the last one completes the payment.
#[test]
fn each_observation_reports_one_saying_and_the_engine_asks_for_the_rest() {
    let (mut runner, toy_boat) = setup_at_unless_prompt(1, true);

    let first = say(&mut runner, 1, 5_000);
    assert_eq!(
        phrases_said(&first.events),
        vec![(P0, "Toy Boat".to_string())],
        "one announcement per observation: {:?}",
        first.events
    );
    assert_eq!(
        owed(&runner),
        1,
        "the re-prompt carries exactly the saying still owed"
    );
    let progress = runner.state().say_payment.as_ref().unwrap();
    assert_eq!((progress.owed, progress.said), (2, 1));
    assert_eq!(
        progress.deadline_ms,
        Some(6_000),
        "the engine stores the published window from the host's own stamp"
    );
    assert_eq!(
        runner.state().objects[&toy_boat].zone,
        Zone::Battlefield,
        "the upkeep is undecided while a saying is still owed"
    );

    let second = say(&mut runner, 2, 5_700);
    assert_eq!(
        phrases_said(&second.events),
        vec![(P0, "Toy Boat".to_string())],
        "the second saying is announced once: {:?}",
        second.events
    );
    assert_eq!(
        runner.state().objects[&toy_boat].zone,
        Zone::Battlefield,
        "saying it once per age counter keeps Toy Boat"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }),
        "the completed payment closes the prompt, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().say_payment.is_none(),
        "a completed payment retires its instance"
    );
}

/// CR 118.12a + Un-card (no CR entry): a saying that arrives after the stored
/// deadline is a pause. The engine — not the client — turns that into the unpaid
/// branch, so Toy Boat is sacrificed.
#[test]
fn a_late_saying_is_a_pause_and_sacrifices_toy_boat() {
    let (mut runner, toy_boat) = setup_at_unless_prompt(1, true);

    let first = say(&mut runner, 1, 1_000);
    assert_eq!(phrases_said(&first.events).len(), 1);
    assert_eq!(owed(&runner), 1);

    // 1_001ms is one millisecond past the stored deadline (2_000).
    let late = say(&mut runner, 2, 2_001);
    assert!(
        phrases_said(&late.events).is_empty(),
        "a late saying is not a saying: {:?}",
        late.events
    );
    assert_eq!(
        runner.state().objects[&toy_boat].zone,
        Zone::Graveyard,
        "pausing between sayings leaves the cost unpaid"
    );
    assert!(
        runner.state().say_payment.is_none(),
        "the retired payment leaves no stale instance"
    );
}

/// CR 118.12a: a client that answers the paid branch with a boolean cannot pay
/// a TIMED spoken cost — the engine has no stamped saying to validate, so the
/// cost is unpaid. This is the direct-client hole the pacing model closes.
#[test]
fn a_boolean_answer_cannot_pay_a_timed_spoken_cost() {
    let (mut runner, toy_boat) = setup_at_unless_prompt(1, true);

    let result = runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the boolean answer is accepted as a decline-equivalent");
    assert!(
        phrases_said(&result.events).is_empty(),
        "no saying was reported: {:?}",
        result.events
    );
    assert_eq!(
        runner.state().objects[&toy_boat].zone,
        Zone::Graveyard,
        "an unverifiable timed payment is not a payment"
    );
}

/// CR 118.12a + Un-card (no CR entry): duplicate and reordered host deliveries
/// are ignored, so latency cannot buy extra sayings or move the deadline
/// backwards.
#[test]
fn out_of_order_and_duplicate_observations_are_ignored() {
    let (mut runner, toy_boat) = setup_at_unless_prompt(1, true);

    say(&mut runner, 2, 900);
    // A frame stamped before the accepted one: neither sequence nor instant is
    // newer, so it changes nothing.
    let stale = say(&mut runner, 1, 400);
    assert!(
        phrases_said(&stale.events).is_empty(),
        "a reordered frame says nothing: {:?}",
        stale.events
    );
    assert_eq!(owed(&runner), 1, "the payment did not move");

    // The retransmit of the accepted frame is equally inert.
    say(&mut runner, 2, 900);
    assert_eq!(owed(&runner), 1);
    assert_eq!(
        runner.state().objects[&toy_boat].zone,
        Zone::Battlefield,
        "the upkeep is still undecided"
    );

    // And the payment still completes normally.
    say(&mut runner, 3, 1_100);
    assert_eq!(runner.state().objects[&toy_boat].zone, Zone::Battlefield);
}

/// Un-card (no CR entry): an observation addressed to another payment instance
/// is ignored, exactly as a click for a finished contest is. A delayed frame
/// from an earlier upkeep cannot count toward this one.
#[test]
fn an_observation_for_another_payment_is_ignored() {
    let (mut runner, _toy_boat) = setup_at_unless_prompt(1, true);

    let live = payment_id(&runner);
    let result = runner
        .act(GameAction::SayPhrase {
            payment: live + 10,
            seq: 1,
            at_ms: 0,
        })
        .expect("a stale frame is ignored, not an error");
    assert!(
        phrases_said(&result.events).is_empty(),
        "a frame for another payment says nothing: {:?}",
        result.events
    );
    assert_eq!(owed(&runner), 2, "the live payment is untouched");
}

/// CR 118.12a: declining (or letting the window lapse) means the cost was not
/// paid, so the cumulative-upkeep sacrifice happens.
#[test]
fn declining_sacrifices_toy_boat() {
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
    assert!(
        runner.state().say_payment.is_none(),
        "declining retires the payment instance"
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

/// Un-card (no CR entry): the payment instance is part of `GameState`, so its
/// identity, accepted count and stored deadline survive a reconnect (a
/// serialize/deserialize round trip). Time is validated against the same
/// deadline after the restore — the engine never restarts the window.
#[test]
fn the_payment_instance_survives_a_serialize_round_trip() {
    let (mut runner, toy_boat) = setup_at_unless_prompt(1, true);
    let first = say(&mut runner, 1, 1_000);
    assert_eq!(phrases_said(&first.events).len(), 1);

    let wire = serde_json::to_string(runner.state()).expect("state serializes");
    let restored: GameState = serde_json::from_str(&wire).expect("state deserializes");
    let payment = restored
        .say_payment
        .as_ref()
        .expect("the instance is part of the persisted state");
    assert_eq!((payment.owed, payment.said), (2, 1));
    assert_eq!(payment.last_seq, Some(1));
    assert_eq!(payment.last_said_ms, Some(1_000));
    assert_eq!(payment.deadline_ms, Some(2_000));

    // The deadline the restored instance carries still decides the next saying:
    // inside it pays, past it does not.
    let mut inside = payment.as_ref().clone();
    assert_eq!(
        inside.observe(
            SayPhraseObservation {
                seq: 2,
                at_ms: 1_999
            },
            Some(1_000)
        ),
        SayPhraseDisposition::Accepted
    );
    let mut late = payment.as_ref().clone();
    assert_eq!(
        late.observe(
            SayPhraseObservation {
                seq: 2,
                at_ms: 2_001
            },
            Some(1_000)
        ),
        SayPhraseDisposition::Lapsed
    );
    // A duplicate of the accepted first saying is inert after the restore too.
    let mut duplicate = payment.as_ref().clone();
    assert_eq!(
        duplicate.observe(
            SayPhraseObservation {
                seq: 1,
                at_ms: 1_000
            },
            Some(1_000)
        ),
        SayPhraseDisposition::Stale
    );
    assert_eq!(
        runner.state().objects[&toy_boat].zone,
        Zone::Battlefield,
        "the pre-restore saying kept the upkeep undecided"
    );
}

/// A fresh instance starts from zero: the per-saying window only starts after
/// the first saying, so a one-saying requirement can never expire before it is
/// made (CR 118.12a).
#[test]
fn a_single_required_saying_never_expires_before_it_is_made() {
    let (mut scenario_runner, toy_boat) = setup_at_unless_prompt(0, true);
    // With no preloaded age counter the tick leaves one, so one saying is owed.
    assert_eq!(owed(&scenario_runner), 1);
    let payment = scenario_runner.state().say_payment.as_ref().unwrap();
    assert_eq!(payment.owed, 1);
    assert!(
        payment.deadline_ms.is_none(),
        "there is no pause to time before the only saying"
    );

    say(&mut scenario_runner, 1, 60_000);
    assert_eq!(
        scenario_runner.state().objects[&toy_boat].zone,
        Zone::Battlefield,
        "the single saying paid the upkeep whenever the player made it"
    );
}

/// A newly minted instance is independent: identities never repeat, so a frame
/// from a previous upkeep cannot bind to a later one.
#[test]
fn successive_payments_have_distinct_identities() {
    let (mut runner, _toy_boat) = setup_at_unless_prompt(1, true);
    let first = payment_id(&runner);

    // Decline this upkeep and start the next one.
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining is accepted");
    runner.advance_to_upkeep();
    runner.resolve_top();

    if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
        let second = payment_id(&runner);
        assert_ne!(first, second, "each payment mints a fresh identity");
    }
}

/// A prompt directly replaced by a NEW payment with the SAME number of sayings
/// owed must not accept an observation addressed to the retired instance. The
/// identity, not the count, distinguishes one payment from the next, so a
/// delayed frame from the old one can never pay the new one.
#[test]
fn a_replaced_payment_rejects_the_retired_instance() {
    let (mut runner, _toy_boat) = setup_at_unless_prompt(1, true);
    let retired = payment_id(&runner);
    let owed_now = owed(&runner);
    assert!(
        owed_now > 1,
        "the two-saying witness for the replacement test"
    );

    // Replace the live instance in place, keeping the prompt and its owed count
    // identical: only the identity changes.
    runner.state_mut().say_payment = Some(Box::new(SayPhrasePayment::new(retired + 1, owed_now)));

    let stale = runner
        .act(GameAction::SayPhrase {
            payment: retired,
            seq: 1,
            at_ms: 1_000,
        })
        .expect("a frame for the retired instance is ignored, not an error");
    assert!(
        phrases_said(&stale.events).is_empty(),
        "the retired instance says nothing: {:?}",
        stale.events
    );
    assert_eq!(owed(&runner), owed_now, "the new payment did not move");

    let fresh = runner
        .act(GameAction::SayPhrase {
            payment: retired + 1,
            seq: 1,
            at_ms: 1_100,
        })
        .expect("the live instance accepts its own observation");
    assert_eq!(phrases_said(&fresh.events).len(), 1);
}

/// `payment: 0` means "the payment this prompt advertises". It binds to the
/// LIVE instance rather than minting a replacement, so a client that omits the
/// id cannot restart the count (and its window) by sending 0 again.
#[test]
fn a_zero_payment_id_binds_to_the_live_instance() {
    let (mut runner, _toy_boat) = setup_at_unless_prompt(1, true);
    let live = payment_id(&runner);

    runner
        .act(GameAction::SayPhrase {
            payment: 0,
            seq: 1,
            at_ms: 1_000,
        })
        .expect("the advertised payment accepts an unnamed observation");
    assert_eq!(
        runner.state().say_payment.as_ref().unwrap().id,
        live,
        "0 binds to the live instance, it does not mint a new one"
    );
    assert_eq!(owed(&runner), 1);

    // The second unnamed saying still binds to the same instance and finishes
    // the payment; a reset would have left two sayings owed again.
    runner
        .act(GameAction::SayPhrase {
            payment: 0,
            seq: 2,
            at_ms: 1_500,
        })
        .expect("the remaining saying is accepted");
    assert!(runner.state().say_payment.is_none());
}

/// The instance's own accounting is a pure function of host observations, so
/// the deadline rule can be pinned without a clock: the window is the gap
/// between sayings, set from each accepted saying's own stamp.
#[test]
fn the_window_is_measured_between_host_stamps() {
    let mut payment = SayPhrasePayment::new(1, 3);
    assert_eq!(
        payment.observe(
            SayPhraseObservation {
                seq: 1,
                at_ms: 10_000
            },
            Some(1_000)
        ),
        SayPhraseDisposition::Accepted
    );
    assert_eq!(payment.deadline_ms, Some(11_000));
    assert_eq!(
        payment.observe(
            SayPhraseObservation {
                seq: 2,
                at_ms: 10_999
            },
            Some(1_000)
        ),
        SayPhraseDisposition::Accepted
    );
    assert_eq!(
        payment.deadline_ms,
        Some(11_999),
        "each accepted saying restarts the window from its own host stamp"
    );
    assert_eq!((payment.owed, payment.said), (3, 2));
    // A re-delivery of an accepted sequence is stale even when its instant is
    // newer: the host sequence, not the timestamp, is the duplicate authority.
    assert_eq!(
        payment.observe(
            SayPhraseObservation {
                seq: 2,
                at_ms: 11_500
            },
            Some(1_000)
        ),
        SayPhraseDisposition::Stale
    );
    assert_eq!((payment.owed, payment.said), (3, 2));
}
