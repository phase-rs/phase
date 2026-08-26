//! Condition-channel twin of the Ashaya entry-flush regression
//! (`ashaya_nontoken_lands.rs`): a static's enabling CONDITION that counts a
//! population must be answered against the post-layer board when the entry
//! escalation gate decides whether an entering object perturbs it.
//!
//! Life and Limb makes all Saprolings Forest lands (layer 4, CR 613.1d);
//! Sylvan Advocate's +2/+2 is gated on "you control six or more lands"
//! (CR 611.3a enabling condition). An entering Saproling is a creature — not a
//! land — at gate time, so a pre-layer probe of "lands you control" would report
//! the count unperturbed even though the post-layer board crosses six.
//!
//! This test pins the CR-correct end-to-end outcome whichever flush arm the
//! board takes. It is deliberately NOT the discriminating test for the
//! escalation gate's condition channel — that is the synthetic fixture named in
//! the per-test comment below, which is constructed to take the incremental
//! path.

use engine::game::scenario::{GameScenario, P0};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const LIFE_AND_LIMB: &str = "All Forests and all Saprolings are 1/1 green \
Saproling creatures and Forest lands in addition to their other types.";
const SYLVAN_ADVOCATE: &str = "Vigilance\nAs long as you control six or more \
lands, this creature and land creatures you control get +2/+2.";

/// CR 611.3a + CR 613.1 + CR 613.1d: with five lands, Life and Limb, and
/// Sylvan Advocate on the battlefield, a Saproling entering becomes the sixth
/// land during the same layer pass, so the Advocate's condition turns on and
/// the Advocate — a PRE-EXISTING recipient — must end the pass at 4/5, not a
/// stale 2/3.
///
/// This asserts the CR-correct end-to-end outcome and nothing about WHICH flush
/// arm produced it: the assertion holds under either arm, and deliberately does
/// not encode a claim about the arm, because such a claim would be prose that no
/// assertion here can keep honest. The discriminating test for the escalation
/// gate's condition channel is the synthetic
/// `condition_gated_anthem_entry_escalates_when_entrant_types_rewritten`
/// fixture (stack.rs entry-flush escalation tests), which asserts escalation
/// directly.
#[test]
fn sylvan_advocate_condition_counts_a_saproling_that_life_and_limb_turns_into_a_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let advocate_id = scenario
        .add_creature_from_oracle(P0, "Sylvan Advocate", 2, 3, SYLVAN_ADVOCATE)
        .id();
    scenario.add_enchantment_from_oracle(P0, "Life and Limb", LIFE_AND_LIMB);
    for i in 0..5 {
        scenario.add_land_from_oracle(P0, &format!("Quiet Wastes {i}"), "");
    }
    let entering = scenario
        .add_creature_to_hand(P0, "Saproling Straggler", 1, 1)
        .with_subtypes(vec!["Saproling"])
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    runner.cast(entering).resolve();

    let advocate = &runner.state().objects[&advocate_id];
    assert_eq!(
        (advocate.power, advocate.toughness),
        (Some(4), Some(5)),
        "the entering Saproling is a Forest land post-layer, lands reach six, \
         and the Advocate's own +2/+2 applies to itself"
    );
}
