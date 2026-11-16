//! Issue #7900: Ignite Memories — "Target player reveals a card at random from
//! their hand. Ignite Memories deals damage to that player equal to that
//! card's mana value." Exactly one random card is revealed and its mana value
//! is dealt as damage.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
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
    let mut hand_values = Vec::new();
    for (i, v) in values.iter().enumerate() {
        let mut b = scenario.add_spell_to_hand_from_oracle(P1, &format!("Filler {i}"), false, "");
        b.with_mana_cost(ManaCost::generic(*v));
        hand_values.push((b.id(), *v));
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
    let outcome = runner.cast(ignite).target_player(P1).resolve();

    // CR 701.20a + CR 120.3a: bind the damage to the one publicly revealed card.
    let reveals: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::CardsRevealed {
                player, card_ids, ..
            } if *player == P1 => Some(card_ids),
            _ => None,
        })
        .collect();
    assert_eq!(reveals.len(), 1, "one reveal instruction resolves");
    assert_eq!(
        reveals[0].len(),
        1,
        "only one card is revealed, not the whole hand"
    );
    let (_, mana_value) = hand_values
        .iter()
        .find(|(id, _)| *id == reveals[0][0])
        .expect("the revealed card belongs to the target player's hand");
    outcome.assert_life_delta(P1, -(*mana_value as i32));
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
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
    let mut hand_values = Vec::new();
    for (i, v) in [1u32, 3, 6].iter().enumerate() {
        let mut b = scenario.add_spell_to_hand_from_oracle(P1, &format!("Filler {i}"), false, "");
        b.with_mana_cost(ManaCost::generic(*v));
        hand_values.push((b.id(), *v));
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
    runner.cast(ignite).target_player(P1).commit();
    // CR 702.40a + CR 707.10c: Storm's copy offers new targets; keep P1.
    runner
        .act(GameAction::PassPriority)
        .expect("pass to opponent");
    runner
        .act(GameAction::PassPriority)
        .expect("resolve Storm trigger");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CopyRetarget { player: P0, .. }
    ));
    runner
        .act(GameAction::KeepAllCopyTargets)
        .expect("keep P1 as the copy target");

    // CR 702.40a + CR 701.20a + CR 120.3a: check each spell's own reveal and
    // damage, even if both random selections happen to choose the same card.
    for spells_remaining in [2, 1] {
        assert_eq!(runner.state().stack.len(), spells_remaining);
        let before = runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P1)
            .unwrap()
            .life;
        let mut events = runner
            .act(GameAction::PassPriority)
            .expect("pass to opponent")
            .events;
        events.extend(
            runner
                .act(GameAction::PassPriority)
                .expect("resolve one spell")
                .events,
        );
        let reveals: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CardsRevealed {
                    player, card_ids, ..
                } if *player == P1 => Some(card_ids),
                _ => None,
            })
            .collect();
        assert_eq!(
            reveals.len(),
            1,
            "each spell resolves one reveal instruction"
        );
        assert_eq!(reveals[0].len(), 1, "each spell reveals exactly one card");
        let (_, mana_value) = hand_values
            .iter()
            .find(|(id, _)| *id == reveals[0][0])
            .expect("each revealed card belongs to the target player's hand");
        let damage: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::DamageDealt {
                    target: TargetRef::Player(P1),
                    amount,
                    ..
                } => Some(*amount),
                _ => None,
            })
            .collect();
        assert_eq!(
            damage,
            vec![*mana_value],
            "damage uses this resolution's revealed card"
        );
        let after = runner
            .state()
            .players
            .iter()
            .find(|p| p.id == P1)
            .unwrap()
            .life;
        assert_eq!(before - after, *mana_value as i32);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
    }
    assert!(
        runner.state().stack.is_empty(),
        "original and copy both finished"
    );
}
