//! Issue #5287 — Samwise the Stouthearted ETB with no eligible graveyard return.
//!
//! Oracle: Flash
//! When Samwise enters, choose up to one target permanent card in your graveyard
//! that was put there from the battlefield this turn. Return it to your hand.
//! Then the Ring tempts you.
//!
//! When zero graveyard cards qualify, the optional target is declined and the
//! bounce anaphor must not fall back to Samwise himself; Ring temptation still
//! resolves.

use engine::game::scenario::{GameScenario, P0};
use engine::game::zones::move_to_zone;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const SAMWISE_ORACLE: &str = "Flash\nWhen Samwise enters, choose up to one target permanent card in your graveyard that was put there from the battlefield this turn. Return it to your hand. Then the Ring tempts you.";

fn white_mana_pool() -> Vec<ManaUnit> {
    vec![
        ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
        ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
    ]
}

#[test]
fn samwise_etb_with_no_graveyard_return_stays_on_battlefield_and_tempts_ring() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, white_mana_pool());
    let samwise = scenario
        .add_creature_to_hand_from_oracle(P0, "Samwise the Stouthearted", 2, 1, SAMWISE_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.cast(samwise).resolve();

    assert_eq!(
        runner.state().objects.get(&samwise).map(|o| o.zone),
        Some(Zone::Battlefield),
        "declining the up-to-one graveyard return must not bounce Samwise to hand"
    );
    assert_eq!(
        runner.state().ring_level.get(&P0).copied(),
        Some(1),
        "Ring temptation must still resolve after a declined optional return"
    );
}

#[test]
fn samwise_etb_returns_creature_put_into_graveyard_from_battlefield_this_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, white_mana_pool());
    let victim = scenario.add_creature(P0, "Fallen Knight", 2, 2).id();
    let samwise = scenario
        .add_creature_to_hand_from_oracle(P0, "Samwise the Stouthearted", 2, 1, SAMWISE_ORACLE)
        .id();

    let mut runner = scenario.build();
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), victim, Zone::Graveyard, &mut events);

    let outcome = runner.cast(samwise).target_object(victim).resolve();

    outcome.assert_zone(&[victim], Zone::Hand);
    outcome.assert_zone(&[samwise], Zone::Battlefield);
    assert_eq!(
        runner.state().ring_level.get(&P0).copied(),
        Some(1),
        "Ring temptation follows a successful optional return"
    );
}
