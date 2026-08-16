//! CR 601.2f regression (runtime cast pipeline): a filtered one-shot "the next
//! [type] spell you cast this turn costs {N} less" reduction (Kadena, Slinking
//! Sorcerer) must be consumed by the first matching cast and must not discount
//! later matching spells that turn.
//!
//! This drives the real cast pipeline — `GameScenario` → `cast().resolve()` →
//! finalize (`apply_pending_spell_cost_reductions` during cost calculation, then
//! `consume_pending_spell_cost_reduction` at finalize) — casting two {2} creature
//! spells with a creature-filtered {1} reduction and proving only the first is
//! discounted. On the old consumer (which removed only *unfiltered* entries) the
//! second cast is also discounted and this test fails.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{TargetFilter, TypedFilter};
use engine::types::game_state::PendingSpellCostReduction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

#[test]
fn filtered_reduction_discounts_only_the_first_matching_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario
        .add_creature_to_hand(P0, "Bear One", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let c2 = scenario
        .add_creature_to_hand(P0, "Bear Two", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut runner = scenario.build();

    // 3 colorless mana: enough for the reduced first cast ({1}) plus the full
    // second cast ({2}).
    for _ in 0..3 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }

    // A creature-filtered one-shot {1} reduction ("the next creature spell you
    // cast this turn costs {1} less").
    runner
        .state_mut()
        .pending_spell_cost_reductions
        .push(PendingSpellCostReduction {
            player: P0,
            amount: 1,
            spell_filter: Some(TargetFilter::Typed(TypedFilter::creature())),
        });

    // Cast 1: {2} - {1} = {1} spent, and the reduction is consumed.
    let before1 = runner.state().players[0].mana_pool.total();
    runner.cast(c1).resolve();
    let spent1 = before1 - runner.state().players[0].mana_pool.total();
    assert_eq!(
        spent1, 1,
        "the first creature spell is discounted by {{1}} (2 - 1)"
    );
    assert!(
        runner.state().pending_spell_cost_reductions.is_empty(),
        "the filtered reduction must be consumed by the first matching cast"
    );

    // Cast 2: full {2} — the one-shot reduction is already gone.
    let before2 = runner.state().players[0].mana_pool.total();
    runner.cast(c2).resolve();
    let spent2 = before2 - runner.state().players[0].mana_pool.total();
    assert_eq!(
        spent2, 2,
        "the second creature spell pays full cost (reduction already consumed)"
    );
}
