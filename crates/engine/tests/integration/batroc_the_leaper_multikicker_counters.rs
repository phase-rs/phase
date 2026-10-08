//! Batroc the Leaper — the enters-with-counter replacement must scale by the
//! DYNAMIC multikicker count, not a fixed `1`.
//!
//! Parser regression (`parse_enters_counter_for_each_suffix` +
//! `parse_for_each_kicker_count`): "a +1/+1 counter on him for each time he was
//! kicked" self-references with a gendered pronoun ("on him … he was kicked").
//! Before the fix both recognizers only accepted the neuter "it"/"this spell"
//! surface, so the for-each scaling clause was dropped and the count collapsed to
//! `Fixed { 1 }` — Batroc always entered with exactly one counter regardless of
//! how many times he was kicked.
//!
//! Built via the `/card-test` recipe: `GameScenario` + the real cast pipeline
//! (`GameAction::CastSpell` -> `DecideOptionalCost` -> resolution), driving the
//! multikicker an exact number of times through the engine so the entering
//! object's `kickers_paid` is populated authentically. The structural reach-guard
//! `assert_batroc_replacement_is_dynamic` proves the replacement parsed with the
//! dynamic `KickerCount`, so a "0 counters" assertion cannot pass vacuously on a
//! card whose replacement failed to parse.
//!
//! REVERT DISCRIMINATOR: `batroc_two_kicks_get_two_counters`. Revert either
//! parser widening and the count reverts to `Fixed { 1 }`, so a Batroc kicked
//! twice enters with 1 counter and this test's `assert_eq!(.., 2)` fails.
//! `batroc_zero_kicks_gets_no_counters` is the second polarity: `Fixed { 1 }`
//! would wrongly give 1 counter to an un-kicked Batroc.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{
    AbilityCost, AdditionalCost, AdditionalCostRepeatability, Effect, KickerVariant, QuantityExpr,
    QuantityRef,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim replacement line from Batroc the Leaper (`data/card-data.json`); the
/// Multikicker keyword line and the ETB damage trigger are omitted so the fixture
/// isolates the enters-with replacement under test. The multikicker is supplied
/// as an explicit additional cost.
const BATROC_ENTERS: &str =
    "Batroc enters with a +1/+1 counter on him for each time he was kicked.";

/// Structural reach-guard: Batroc really parsed an enters-with replacement whose
/// counter count is the dynamic `KickerCount`, not `Fixed { 1 }`. Without this, a
/// "0 counters" assertion would pass just as well on a card whose replacement
/// failed to parse at all (the `/card-test` foot-gun #6 defence).
fn assert_batroc_replacement_is_dynamic(runner: &GameRunner, batroc: ObjectId) {
    let obj = &runner.state().objects[&batroc];
    let def = obj
        .replacement_definitions
        .first()
        .expect("Batroc must publish an enters-with replacement");
    let effect = &*def
        .execute
        .as_ref()
        .expect("Batroc's enters-with replacement must carry an execute ability")
        .effect;
    assert!(
        matches!(
            effect,
            Effect::PutCounter {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::KickerCount
                },
                ..
            }
        ),
        "Batroc's enters-with replacement must scale by the dynamic kicker count, got {effect:?}"
    );
}

/// Cast Batroc paying the multikicker exactly `kicks` times through the real
/// pipeline and return the number of +1/+1 counters he enters the battlefield
/// with. When `decoy_kicks` is set, an unrelated creature that was kicked that
/// many times is already on the battlefield — a multi-authority hostile fixture
/// proving Batroc reads his OWN `kickers_paid`, never another permanent's.
fn batroc_counters_after_kicks(kicks: u32, decoy_kicks: Option<u32>) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let batroc = scenario
        .add_creature_to_hand_from_oracle(P0, "Batroc the Leaper", 2, 2, BATROC_ENTERS)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Red],
        })
        // Multikicker {2}: a repeatable optional additional cost (CR 702.33c).
        .with_additional_cost(AdditionalCost::Kicker {
            costs: vec![AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 2,
                    shards: vec![],
                },
            }],
            repeatability: AdditionalCostRepeatability::Repeatable,
        })
        .id();

    // A generous, single-color pool so the auto-payer can always cover the base
    // {1}{R} plus several {2} kicks; the exact kick count is controlled below by
    // the `DecideOptionalCost` sequence, not by mana starvation.
    let decoy = decoy_kicks.map(|_| scenario.add_vanilla(P0, 1, 1));
    for _ in 0..8 {
        scenario.add_basic_land(P0, ManaColor::Red);
    }

    let mut runner = scenario.build();

    if let (Some(id), Some(n)) = (decoy, decoy_kicks) {
        // Seed the decoy's kick count directly; it never leaks into Batroc's own
        // entering-object read.
        runner
            .state_mut()
            .objects
            .get_mut(&id)
            .unwrap()
            .kickers_paid = vec![KickerVariant::First; n as usize];
    }

    assert_batroc_replacement_is_dynamic(&runner, batroc);

    let card = runner.state().objects[&batroc].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: batroc,
            card_id: card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts Batroc");

    // Pay the repeatable multikicker exactly `kicks` times, then decline. The
    // engine re-raises `OptionalCostChoice` after each accepted kick, so a
    // running counter drives the pay/decline decision.
    let mut paid = 0u32;
    while let WaitingFor::OptionalCostChoice { .. } = &runner.state().waiting_for {
        let pay = paid < kicks;
        runner
            .act(GameAction::DecideOptionalCost { pay })
            .expect("P0 decides the multikicker");
        if pay {
            paid += 1;
        } else {
            break;
        }
    }
    assert_eq!(
        paid, kicks,
        "the engine must offer the multikicker enough times to pay it {kicks} time(s)"
    );

    // Resolve the spell onto the battlefield; the enters-with replacement applies
    // as Batroc enters (CR 614.12), reading his own kicker count.
    while runner.state().objects.get(&batroc).map(|o| o.zone) != Some(Zone::Battlefield) {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority to resolve Batroc");
            }
            other => panic!("unexpected waiting state while resolving Batroc: {other:?}"),
        }
    }

    runner.state().objects[&batroc]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// PRIMARY REVERT DISCRIMINATOR. Kicked twice, Batroc enters with two +1/+1
/// counters. Revert either parser widening → `Fixed { 1 }` → 1 counter → fails.
#[test]
fn batroc_two_kicks_get_two_counters() {
    assert_eq!(
        batroc_counters_after_kicks(2, None),
        2,
        "Batroc kicked twice must enter with two +1/+1 counters"
    );
}

/// Second polarity. Un-kicked, Batroc enters with NO counters. Revert to
/// `Fixed { 1 }` and an un-kicked Batroc wrongly gains 1 counter → fails. Paired
/// reach-guard: `assert_batroc_replacement_is_dynamic` proves the replacement
/// parsed, so this 0 is a real kicker-count read, not a parse failure.
#[test]
fn batroc_zero_kicks_gets_no_counters() {
    assert_eq!(
        batroc_counters_after_kicks(0, None),
        0,
        "an un-kicked Batroc must enter with no +1/+1 counters"
    );
}

/// Multi-authority isolation (CR 201.5 self-reference): a different creature that
/// was kicked three times is on the battlefield when Batroc, kicked twice,
/// enters. Batroc must read HIS OWN kicker count (2), never the decoy's (3).
#[test]
fn batroc_reads_own_kicks_not_another_permanents() {
    assert_eq!(
        batroc_counters_after_kicks(2, Some(3)),
        2,
        "Batroc must count his own kicks (2), not a decoy permanent's (3)"
    );
}
