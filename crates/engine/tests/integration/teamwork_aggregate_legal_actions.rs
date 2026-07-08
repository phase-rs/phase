//! Teamwork aggregate tap cost — AI/MP legal-action enumeration must offer
//! power-thresholded creature SUBSETS, not a fixed full-set cardinality.
//!
//! PR #3916 parameterizes `AbilityCost::TapCreatures` with an aggregate
//! `TotalPower` requirement — the Teamwork form "tap any number of creatures you
//! control with total power N or more" (CR 601.2b/f), the same shape as Crew
//! (CR 702.122a) and Saddle (CR 702.171a). The payment validator
//! (`handle_tap_creatures_for_spell_cost`) already accepts any subset whose
//! summed current power meets the threshold, but two AI-facing seams treated the
//! aggregate form like the fixed-count form: candidate generation
//! (`ai_support::candidates`) emitted only a single full-cardinality
//! `[eligible.len()]` selection, and the legality predicate
//! (`ai_support::cheap_reject_candidate`) validated with exact-cardinality
//! `Some(count)` semantics. Together these left `legal_actions` (the AI /
//! multiplayer-server legal-action set) unable to present any legal minimal
//! subset, so AI and MP players could never pay Teamwork unless tapping every
//! eligible creature.
//!
//! These tests drive the real cast/payment pipeline to the `PayCost`
//! tap-creatures window and assert `legal_actions` offers a sub-full-size
//! qualifying subset and rejects a sub-threshold one. They fail against the
//! pre-fix fixed-cardinality behavior, and the current-power test fails if base
//! P/T (rather than layer-evaluated power) is summed.

use engine::ai_support::legal_actions;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, PayCostKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

/// Teamwork 3 spell with a no-target body and {0} mana cost so the cast reaches
/// the optional Teamwork cost without any target or mana detours.
const TEAMWORK_3: &str = "Teamwork 3 (As an additional cost to cast this spell, you may tap any number of creatures you control with total power 3 or more.)\nYou gain 1 life.";

fn select(cards: Vec<ObjectId>) -> GameAction {
    GameAction::SelectCards { cards }
}

fn offers(actions: &[GameAction], cards: &[ObjectId]) -> bool {
    actions
        .iter()
        .any(|a| matches!(a, GameAction::SelectCards { cards: c } if c.as_slice() == cards))
}

/// Cast the Teamwork spell, pay the optional Teamwork cost, and stop at the
/// `PayCost` tap-creatures window. Panics if that window is never reached.
fn cast_and_reach_tap_paycost(runner: &mut GameRunner, spell: ObjectId) -> i32 {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the Teamwork spell must be accepted");

    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalCostChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalCost { pay: true })
                    .expect("opting to pay teamwork must be accepted");
            }
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalizing {0} mana cost must be accepted");
            }
            WaitingFor::PayCost {
                kind:
                    PayCostKind::TapCreatures {
                        aggregate: Some(aggregate),
                    },
                ..
            } => return aggregate.value,
            other => panic!("unexpected window before tap-creatures payment: {other:?}"),
        }
    }
    panic!("never reached the tap-creatures PayCost window");
}

fn setup(powers: [(i32, i32); 3]) -> (GameRunner, [ObjectId; 3], ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c0 = scenario
        .add_creature(P0, "Alpha", powers[0].0, powers[0].1)
        .id();
    let c1 = scenario
        .add_creature(P0, "Bravo", powers[1].0, powers[1].1)
        .id();
    let c2 = scenario
        .add_creature(P0, "Charlie", powers[2].0, powers[2].1)
        .id();
    let mut builder = scenario.add_spell_to_hand_from_oracle(P0, "Squad Up", false, TEAMWORK_3);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 0,
    });
    let spell = builder.id();
    (scenario.build(), [c0, c1, c2], spell)
}

/// With three eligible creatures of power [3, 1, 1] and Teamwork 3, the engine
/// must offer the single power-3 creature alone (total power 3 >= 3) as a legal
/// payment, and must NOT offer a lone power-1 creature (total power 1 < 3).
///
/// Pre-fix, candidate generation emitted only the full `[c0, c1, c2]` selection
/// and the legality predicate rejected anything but exactly three creatures, so
/// the `offers(&actions, &[c0])` assertion below is false. It passes only with
/// the subset fix.
#[test]
fn teamwork_aggregate_offers_minimal_qualifying_subset() {
    let (mut runner, [c0, c1, _c2], spell) = setup([(3, 3), (1, 1), (1, 1)]);
    let threshold = cast_and_reach_tap_paycost(&mut runner, spell);
    assert_eq!(
        threshold, 3,
        "Teamwork 3 surfaces an aggregate threshold of 3"
    );

    let actions = legal_actions(runner.state());
    assert!(
        offers(&actions, &[c0]),
        "the single power-3 creature alone (total 3 >= 3) must be an offered legal payment, \
         got {actions:?}"
    );
    assert!(
        !offers(&actions, &[c1]),
        "a lone power-1 creature (total 1 < 3) is sub-threshold and must NOT be offered"
    );

    // The engine accepts the qualifying minimal subset through the real pipeline.
    runner
        .act(select(vec![c0]))
        .expect("engine must accept the qualifying single-creature subset");
    assert!(
        runner.state().objects[&c0].tapped,
        "the chosen power-3 creature must be tapped"
    );
    assert!(
        !runner.state().objects[&c1].tapped,
        "minimal-subset payment must not tap the other creatures"
    );
}

/// A sub-threshold selection (one power-1 creature, total 1 < 3) is rejected by
/// the real payment pipeline — a correctness guard alongside the legal-action
/// enumeration above.
#[test]
fn teamwork_aggregate_pipeline_rejects_subthreshold_subset() {
    let (mut runner, [_c0, c1, _c2], spell) = setup([(3, 3), (1, 1), (1, 1)]);
    cast_and_reach_tap_paycost(&mut runner, spell);
    assert!(
        runner.act(select(vec![c1])).is_err(),
        "tapping a single power-1 creature (total 1 < 3) must be rejected"
    );
}

/// The aggregate threshold is measured against CURRENT (layer-evaluated) power,
/// not base P/T. A base-2 creature carrying a +1/+1 counter has current power 3,
/// so it alone satisfies Teamwork 3 — and must be offered as a single-creature
/// payment. If base power (2) were summed, no single-creature cover would exist
/// and this single-element subset would not be offered.
#[test]
fn teamwork_aggregate_subset_counts_current_not_base_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c0 = scenario.add_creature(P0, "Boosted", 2, 2).id();
    let c1 = scenario.add_creature(P0, "Small A", 1, 1).id();
    let _c2 = scenario.add_creature(P0, "Small B", 1, 1).id();
    scenario.with_counter(c0, CounterType::Plus1Plus1, 1);
    let mut builder = scenario.add_spell_to_hand_from_oracle(P0, "Squad Up", false, TEAMWORK_3);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![],
        generic: 0,
    });
    let spell = builder.id();
    let mut runner = scenario.build();

    // Apply continuous effects so the +1/+1 counter is folded into current power.
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&c0].power,
        Some(3),
        "current power must reflect the +1/+1 counter (base 2 + 1)"
    );

    cast_and_reach_tap_paycost(&mut runner, spell);
    let actions = legal_actions(runner.state());
    assert!(
        offers(&actions, &[c0]),
        "the counter-boosted creature (current power 3 >= 3) must be offered alone, got {actions:?}"
    );
    assert!(
        !offers(&actions, &[c1]),
        "a lone power-1 creature (total 1 < 3) must NOT be offered"
    );

    runner
        .act(select(vec![c0]))
        .expect("engine must accept the counter-boosted single-creature subset");
    assert!(runner.state().objects[&c0].tapped);
}
