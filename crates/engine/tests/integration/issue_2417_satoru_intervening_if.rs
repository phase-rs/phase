//! Regression for issue #2417: Satoru, the Infiltrator draws on normal creature casts.
//!
//! https://github.com/phase-rs/phase/issues/2417
//!
//! Oracle: "Whenever Satoru and/or one or more other nontoken creatures you control
//! enter, if none of them were cast or no mana was spent to cast them, draw a card."

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const SATORU_ORACLE: &str = "Menace\nWhenever Satoru and/or one or more other nontoken \
creatures you control enter, if none of them were cast or no mana was spent to cast them, \
draw a card.";

fn hand_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

#[test]
fn satoru_does_not_draw_when_creature_cast_for_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let satoru = scenario
        .add_creature_to_hand_from_oracle(P0, "Satoru, the Infiltrator", 2, 3, SATORU_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
            generic: 0,
        })
        .id();

    let grizzly = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();

    // Keep a spare card in hand so casting Grizzly does not empty the hand;
    // a mistaken Satoru draw is then observable as +1 card.
    scenario.add_creature_to_hand(P0, "Hand Decoy", 1, 1);

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, satoru, false, vec![]),
            ManaUnit::new(ManaType::Black, satoru, false, vec![]),
            ManaUnit::new(ManaType::Green, grizzly, false, vec![]),
            ManaUnit::new(ManaType::Green, grizzly, false, vec![]),
        ],
    );

    for name in ["Draw 1", "Draw 2", "Draw 3"] {
        scenario.add_card_to_library_top(P0, name);
    }

    let mut runner = scenario.build();
    let hand_before = hand_count(&runner, P0);

    runner.cast(satoru).resolve();
    let hand_after_satoru = hand_count(&runner, P0);
    assert_eq!(
        hand_after_satoru,
        hand_before - 1,
        "casting Satoru itself must not satisfy the intervening-if draw"
    );

    runner.cast(grizzly).resolve();
    assert_eq!(
        hand_count(&runner, P0),
        hand_after_satoru - 1,
        "casting another creature for mana must not trigger Satoru's draw"
    );
}

#[test]
fn satoru_draws_when_creature_cast_for_zero_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let satoru = scenario
        .add_creature_to_hand_from_oracle(P0, "Satoru, the Infiltrator", 2, 3, SATORU_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue, ManaCostShard::Black],
            generic: 0,
        })
        .id();

    let ornithopter = scenario
        .add_creature_to_hand(P0, "Ornithopter", 0, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 0,
        })
        .id();

    // Keep a spare card in hand so the zero-mana creature's net cast/draw delta
    // is measured against a non-empty hand.
    scenario.add_creature_to_hand(P0, "Hand Decoy", 1, 1);

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, satoru, false, vec![]),
            ManaUnit::new(ManaType::Black, satoru, false, vec![]),
        ],
    );

    for name in ["Draw 1", "Draw 2", "Draw 3"] {
        scenario.add_card_to_library_top(P0, name);
    }

    let mut runner = scenario.build();
    runner.cast(satoru).resolve();
    let hand_after_satoru = hand_count(&runner, P0);

    runner.cast(ornithopter).resolve();
    assert_eq!(
        hand_count(&runner, P0),
        hand_after_satoru,
        "casting a {{0}} creature must satisfy Satoru's no-mana-spent intervening-if and draw one card"
    );
}
