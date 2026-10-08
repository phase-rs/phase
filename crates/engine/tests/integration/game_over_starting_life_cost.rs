//! Game Over's cost reduction compares each player's life to that player's
//! own starting-life baseline (CR 904.5: the Archenemy starts at 40).

use engine::game::casting::display_spell_cost;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

const GAME_OVER_ORACLE: &str = "This spell costs {2} less to cast if a player's life total is less than or equal to half their starting life total.\nDestroy all creatures.";

fn printed_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        generic: 3,
    }
}

fn discounted_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        generic: 1,
    }
}

fn discounted_payment() -> Vec<ManaUnit> {
    [ManaType::Colorless, ManaType::Black, ManaType::Black]
        .into_iter()
        .map(|kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

fn game_over_scenario(archenemy_life: i32) -> (GameScenario, ObjectId) {
    let mut format = FormatConfig::archenemy();
    format.archenemy_player = Some(P1);
    let mut scenario = GameScenario::new_with_format(format, 3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Keep the heroes above their own half-baseline so only the Archenemy can
    // make this existential condition true.
    scenario
        .with_life(P0, 21)
        .with_life(P1, archenemy_life)
        .with_life(PlayerId(2), 21);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Game Over", false, GAME_OVER_ORACLE)
        .with_mana_cost(printed_cost())
        .id();
    (scenario, spell)
}

#[test]
fn game_over_cast_cost_uses_each_players_starting_life_and_inclusive_boundary() {
    // At 15, the Archenemy qualifies against half of 40; a controller-bound
    // threshold (half of 20) would not reduce the cost.
    let (mut scenario, spell) = game_over_scenario(15);
    scenario.with_mana_pool(P0, discounted_payment());
    let mut runner = scenario.build();
    // The Archenemy scenario starts with P1 active; P0 owns this spell and
    // must have priority for the cast pipeline to accept the action.
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    assert_eq!(
        display_spell_cost(runner.state(), P0, spell),
        Some(discounted_cost())
    );
    let outcome = runner.cast(spell).resolve();
    assert_eq!(
        outcome.zone_of(spell),
        Zone::Graveyard,
        "the real cast pipeline must accept exactly the discounted cost"
    );

    // Game Over says less than OR EQUAL: exactly half of 40 still qualifies.
    let (scenario, spell) = game_over_scenario(20);
    let runner = scenario.build();
    assert_eq!(
        display_spell_cost(runner.state(), P0, spell),
        Some(discounted_cost())
    );

    // One above half does not qualify; no player in this fixture qualifies.
    let (scenario, spell) = game_over_scenario(21);
    let runner = scenario.build();
    assert_eq!(
        display_spell_cost(runner.state(), P0, spell),
        Some(printed_cost())
    );
}
