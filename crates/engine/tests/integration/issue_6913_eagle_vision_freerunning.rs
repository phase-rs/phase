//! Regression for issue #6913: commander combat damage enables Eagle Vision's
//! Freerunning alternative cost.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const EAGLE_VISION_ORACLE: &str = "Freerunning {1}{U} (You may cast this spell for its freerunning cost if you dealt combat damage to a player this turn with an Assassin or commander.)\nDraw three cards.";

/// CR 702.173a: A commander controlled by the caster dealing combat damage
/// this turn permits paying Freerunning's alternative cost.
#[test]
fn commander_combat_damage_enables_eagle_vision_freerunning() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["First Draw", "Second Draw", "Third Draw"]);
    let commander = scenario.add_creature(P0, "Test Commander", 2, 2).id();
    let eagle_vision = scenario
        .add_spell_to_hand(P0, "Eagle Vision", false)
        .from_oracle_text_with_keywords(&["freerunning:{1}{U}"], EAGLE_VISION_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Blue],
        })
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&commander)
        .expect("commander exists")
        .is_commander = true;

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(commander, AttackTarget::Player(P1))])
        .expect("declare commander attack");
    runner.combat_damage();
    assert!(
        runner
            .state()
            .assassin_or_commander_dealt_combat_damage_this_turn
            .contains(&P0),
        "commander combat damage must grant Freerunning permission"
    );

    runner.advance_to_phase(Phase::PostCombatMain);
    for _ in 0..2 {
        let _ = runner.state_mut().add_mana_to_pool(
            P0,
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        );
    }
    let committed = runner.cast(eagle_vision).commit();
    assert_eq!(
        committed.state().players[0].mana_pool.total(),
        0,
        "the two available blue mana must pay Eagle Vision's {{1}}{{U}} Freerunning cost"
    );

    let outcome = committed.resolve();
    outcome.assert_hand_drawn(P0, 3);
}
