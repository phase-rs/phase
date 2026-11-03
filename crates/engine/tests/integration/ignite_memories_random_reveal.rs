//! Issue #7900: Ignite Memories — "Target player reveals a card at random from
//! their hand. Ignite Memories deals damage to that player equal to that
//! card's mana value." Exactly one random card is revealed and its mana value
//! is dealt as damage.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

// Verbatim Oracle text (client/public/card-data.json, key `ignite memories`).
const IGNITE_MEMORIES: &str = "Target player reveals a card at random from their hand. Ignite Memories deals damage to that player equal to that card's mana value.\nStorm (When you cast this spell, copy it for each spell cast before it this turn. You may choose new targets for the copies.)";

#[test]
fn reveals_one_random_card_and_deals_its_mana_value() {
    // Distinct mana values so the damage identifies which card was revealed.
    let values = [1u32, 3, 6];
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..5)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    let mut hand_ids = Vec::new();
    for (i, v) in values.iter().enumerate() {
        let mut b = scenario.add_spell_to_hand_from_oracle(P1, &format!("Filler {i}"), false, "");
        b.with_mana_cost(ManaCost::generic(*v));
        hand_ids.push(b.id());
    }
    let ignite = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Ignite Memories", false, IGNITE_MEMORIES);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 4,
        });
        b.id()
    };
    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    let before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .life;

    runner.cast(ignite).target_player(P1).resolve();

    let after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .life;
    let dmg = before - after;
    assert!(
        values.iter().any(|v| *v as i32 == dmg),
        "damage {dmg} must equal the mana value of exactly one hand card"
    );
    let revealed = hand_ids
        .iter()
        .filter(|id| runner.state().revealed_cards.contains(id))
        .count();
    assert_eq!(revealed, 1, "only one card is revealed, not the whole hand");
}

/// CR 702.40a: with one prior spell, Storm adds one copy; the original and the
/// copy each reveal a random card and each deal damage equal to its mana value.
#[test]
fn storm_copy_also_reveals_a_random_card_and_deals_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..7)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    for (i, v) in [2u32, 2, 2].iter().enumerate() {
        let mut b = scenario.add_spell_to_hand_from_oracle(P1, &format!("Filler {i}"), false, "");
        b.with_mana_cost(ManaCost::generic(*v));
    }
    let bolt = scenario.add_bolt_to_hand(P0);
    let ignite = {
        let mut b = scenario.add_spell_to_hand(P0, "Ignite Memories", false);
        b.from_oracle_text_with_keywords(&["Storm"], IGNITE_MEMORIES);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 4,
        });
        b.id()
    };
    let mut runner = scenario.build();
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.cast(bolt).target_player(P1).resolve();
    let before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .life;

    runner.cast(ignite).target_player(P1).commit();
    // CR 702.40a + CR 707.10c: Storm's copy offers new targets; keep P1.
    for _ in 0..8 {
        match runner.state().waiting_for {
            WaitingFor::CopyRetarget { .. } => {
                runner.act(GameAction::KeepAllCopyTargets).expect("keep");
            }
            _ if runner.state().stack.is_empty() => break,
            _ => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
        }
    }
    assert!(runner.state().stack.is_empty(), "stack fully resolved");

    let after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .life;
    assert_eq!(before - after, 4, "original + Storm copy each deal 2");
}
