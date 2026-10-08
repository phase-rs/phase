//! Runtime cast-pipeline coverage for the `"and with <keyword>"` entry rider
//! printed alongside an enters-with-counters clause (CR 614.1c), the shape the
//! Invasion kicker cycle uses: "If this creature was kicked, it enters with
//! three +1/+1 counters on it and with trample" (Kavu Titan).
//!
//! Before the fix, `parse_enters_with_counters` composed only the sibling
//! `"enters tapped"` rider, so the keyword clause was never read at all — nine
//! cards entered with their counters and silently without their granted
//! ability. Two parser tests (`kicked_enters_with_counter`,
//! `kicked_with_specific_cost_enters_with_counters`) asserted only the counter
//! and so stayed green through the whole gap; both now assert the keyword too.
//!
//! CR 611.2a is why the grant is modeled as a continuous `AddKeyword` with
//! `Duration::UntilHostLeavesPlay` rather than a one-shot effect: unlike
//! tapping, the keyword is a characteristic the permanent must KEEP while it
//! remains on the battlefield.
//!
//! Built via the `/card-test` recipe: `GameScenario` + a real kicker payment
//! through `DecideOptionalCost`, so the entering object's `kickers_paid` is
//! populated authentically rather than seeded. Both polarities are covered, and
//! each asserts the +1/+1 counters alongside the keyword so that a spell which
//! never resolved cannot satisfy either assertion vacuously.
//!
//! REVERT DISCRIMINATOR: `kavu_titan_kicked_enters_with_trample`. Remove the
//! `compose_enters_with_keyword_grant` call from `parse_enters_with_counters`
//! and the trample grant disappears while the counters still land, so the
//! keyword assertion fails and the counter reach-guard still passes.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{AbilityCost, AdditionalCost, AdditionalCostRepeatability, Effect};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Kavu Titan {1}{G} 2/2 — the enters-with line verbatim. The `Kicker {2}{G}`
/// keyword line itself is supplied structurally via `with_additional_cost` so
/// the cost is paid through the real optional-cost flow.
const KAVU_TITAN: &str =
    "If this creature was kicked, it enters with three +1/+1 counters on it and with trample.";

/// Cast Kavu Titan, paying the kicker or declining it, and return the resolved
/// battlefield object.
fn cast_kavu_titan(kicked: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let titan = scenario
        .add_creature_to_hand_from_oracle(P0, "Kavu Titan", 2, 2, KAVU_TITAN)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        // CR 702.33a: Kicker {2}{G} — a non-repeatable optional additional cost.
        .with_additional_cost(AdditionalCost::Kicker {
            costs: vec![AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 2,
                    shards: vec![ManaCostShard::Green],
                },
            }],
            repeatability: AdditionalCostRepeatability::Once,
        })
        .id();

    for _ in 0..8 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }

    let mut runner = scenario.build();

    // Structural reach-guard: the card must parse cleanly, so a negative
    // keyword assertion cannot be satisfied by an upstream parse failure.
    assert!(
        !runner.state().objects[&titan]
            .abilities
            .iter()
            .any(|a| matches!(&*a.effect, Effect::Unimplemented { .. })),
        "Kavu Titan must parse with zero Effect::Unimplemented, got {:?}",
        runner.state().objects[&titan].abilities
    );

    let card = runner.state().objects[&titan].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: titan,
            card_id: card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts Kavu Titan");

    // Kicker {2}{G} is `Once`, so the engine offers the choice exactly once —
    // an `if let`, unlike the multikicker loop in the Batroc test.
    assert!(
        matches!(
            &runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "the engine must offer the kicker choice, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalCost { pay: kicked })
        .expect("P0 decides the kicker");

    while runner.state().objects.get(&titan).map(|o| o.zone) != Some(Zone::Battlefield) {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve Kavu Titan");
            }
            other => panic!("unexpected waiting state while resolving Kavu Titan: {other:?}"),
        }
    }

    (runner, titan)
}

fn counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// PRIMARY REVERT DISCRIMINATOR. Kicked, so both halves of the one replacement
/// apply: three +1/+1 counters AND trample. The counter assertion is the
/// reach-guard — it proves the replacement fired at all, so a failing trample
/// assertion means the rider specifically was dropped.
#[test]
fn kavu_titan_kicked_enters_with_trample() {
    let (runner, titan) = cast_kavu_titan(true);
    assert_eq!(
        counters(&runner, titan),
        3,
        "reach-guard: the kicked replacement must place three +1/+1 counters — if this is 0 \
         the replacement never applied and the trample assertion below would be vacuous"
    );
    assert!(
        runner.state().objects[&titan].has_keyword(&Keyword::Trample),
        "CR 614.1c: the \"and with trample\" rider is part of the same replacement as the \
         counters, so a kicked Kavu Titan must have trample"
    );
}

/// Opposite polarity: not kicked, so NEITHER half applies. Pairs with the test
/// above so the trample assertion there is shown to be a real gate decision
/// rather than an unconditional grant.
#[test]
fn kavu_titan_unkicked_gets_neither_counters_nor_trample() {
    let (runner, titan) = cast_kavu_titan(false);
    assert_eq!(
        counters(&runner, titan),
        0,
        "an unkicked Kavu Titan must not get the gated counters"
    );
    assert!(
        !runner.state().objects[&titan].has_keyword(&Keyword::Trample),
        "the trample rider is gated on kicker (CR 702.33d), so an unkicked Kavu Titan must \
         NOT have trample — a grant that ignores the gate would fail here"
    );
}
