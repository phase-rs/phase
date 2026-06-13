//! Runtime reproduction for #2384 — Molten-Core Maestro.
//!
//! Opus — "Whenever you cast an instant or sorcery spell, put a +1/+1 counter
//! on this creature. If five or more mana was spent to cast that spell, add an
//! amount of {R} equal to this creature's power."
//!
//! The two clauses form a SequentialSibling chain (PutCounter, then mana for
//! each [power]). The mana amount must read the creature's CURRENT power AFTER
//! the +1/+1 counter resolved (CR 608.2 sequential resolution; CR 613 layers).

use engine::game::scenario::{GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const MAESTRO: &str = "Whenever you cast an instant or sorcery spell, put a +1/+1 counter on this creature. If five or more mana was spent to cast that spell, add an amount of {R} equal to this creature's power.";

#[test]
fn mana_equals_post_counter_power() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Molten-Core Maestro: base 4/4. After the +1/+1 counter it is 5/5.
    scenario.add_creature_from_oracle(P0, "Molten-Core Maestro", 4, 4, MAESTRO);

    // A five-mana sorcery to cast. Fund the pool with five red mana so exactly
    // five mana is spent (satisfying the "five or more mana" Opus rider).
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Five Drop", false, "Draw a card.")
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]); 5],
    );

    let mut runner = scenario.build();

    runner.cast(spell).resolve();
    runner.advance_until_stack_empty();

    let red = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .map(|p| p.mana_pool.count_color(ManaType::Red))
        .unwrap_or(0);
    assert_eq!(
        red, 5,
        "Maestro should add R equal to its CURRENT power after the +1/+1 \
         counter (5/5 → 5 R), not its pre-counter power (4)"
    );
}
