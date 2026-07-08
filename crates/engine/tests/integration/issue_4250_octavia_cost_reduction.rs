//! Regression for GitHub issue #4250 — Octavia, Living Thesis was castable on
//! turn two for just {U}{U}.
//!
//! Octavia's cost is {8}{U}{U} with the static "This spell costs {8} less to
//! cast if you have eight or more instant and/or sorcery cards in your
//! graveyard." The "you have N or more instant and/or sorcery cards in your
//! graveyard" gate was dropped at parse time (the `and/or` multi-type collapsed
//! to no types), so the {8} reduction applied unconditionally — Octavia cost
//! {U}{U} with an empty graveyard.
//!
//! CR 601.2f: a conditional self cost reduction applies only while its gate
//! holds. With fewer than eight instant/sorcery cards in the graveyard the full
//! {8}{U}{U} is due; with eight or more, the {8} reduction applies.
//!
//! The card is built from Oracle text so the live parser (not the committed
//! pre-parsed card-data fixture) supplies the cost-reduction static.

use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameScenario, P0};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const OCTAVIA_COST_REDUCTION: &str = "This spell costs {8} less to cast if you have eight or more \
instant and/or sorcery cards in your graveyard.";

/// {8}{U}{U}.
fn octavia_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
        generic: 8,
    }
}

/// {U}{U} only — enough for Octavia *iff* the {8} reduction is active.
fn two_blue(owner: engine::types::identifiers::ObjectId) -> Vec<ManaUnit> {
    vec![
        ManaUnit::new(ManaType::Blue, owner, false, vec![]),
        ManaUnit::new(ManaType::Blue, owner, false, vec![]),
    ]
}

fn build_octavia(scenario: &mut GameScenario) -> engine::types::identifiers::ObjectId {
    scenario
        .add_creature_to_hand(P0, "Octavia, Living Thesis", 5, 5)
        .with_mana_cost(octavia_cost())
        .from_oracle_text(OCTAVIA_COST_REDUCTION)
        .id()
}

#[test]
fn octavia_not_castable_for_two_blue_with_empty_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let octavia = build_octavia(&mut scenario);
    scenario.with_mana_pool(P0, two_blue(octavia));

    let runner = scenario.build();

    assert!(
        !can_cast_object_now(runner.state(), P0, octavia),
        "Octavia must cost the full {{8}}{{U}}{{U}} with an empty graveyard — the {{8}} reduction \
         must NOT apply without eight instant/sorcery cards in the graveyard"
    );
}

#[test]
fn octavia_castable_for_two_blue_with_eight_instants_in_graveyard() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let octavia = build_octavia(&mut scenario);
    // Eight instant cards in P0's graveyard satisfy the cost-reduction gate.
    for i in 0..8 {
        scenario.add_spell_to_graveyard(P0, &format!("Bolt {i}"), true);
    }
    scenario.with_mana_pool(P0, two_blue(octavia));

    let runner = scenario.build();

    assert!(
        can_cast_object_now(runner.state(), P0, octavia),
        "with eight instant cards in the graveyard the {{8}} reduction applies, so Octavia is \
         castable for {{U}}{{U}}"
    );
}
