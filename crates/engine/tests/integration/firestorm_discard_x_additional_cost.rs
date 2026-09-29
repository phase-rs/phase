//! Firestorm {R}: "As an additional cost to cast this spell, discard X cards.
//! Firestorm deals X damage to each of X targets." (current Oracle wording)
//!
//! Same "discard X cards" additional cost as Restless Dreams: X is announced
//! for the cost (CR 107.3a + CR 601.2b), sizes the target set (CR 601.2c), is
//! paid by discarding (CR 601.2h + CR 701.9a) and is the damage dealt to each
//! target (CR 120.3). The parser used to read X as a fixed 0 and the spell was
//! never castable.

use engine::ai_support::legal_actions;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORACLE: &str = "As an additional cost to cast this spell, discard X cards.\n\
                      Firestorm deals X damage to each of X targets.";

struct Board {
    runner: engine::game::scenario::GameRunner,
    spell: ObjectId,
    creatures: Vec<ObjectId>,
    hand_cards: Vec<ObjectId>,
}

fn board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(P0, "Firestorm", true, ORACLE);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 0,
        });
        b.id()
    };
    let creatures = (0..2)
        .map(|i| {
            scenario
                .add_creature(P1, &format!("Target Ogre {i}"), 3, 3)
                .id()
        })
        .collect();
    let hand_cards = (0..3)
        .map(|i| scenario.add_card_to_hand(P0, &format!("Spare Card {i}")))
        .collect();
    let mut runner = scenario.build();
    let unit = ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]);
    runner.state_mut().players[0].mana_pool.add(unit);
    Board {
        runner,
        spell,
        creatures,
        hand_cards,
    }
}

#[test]
fn firestorm_is_offered_as_castable() {
    let b = board();
    let actions = legal_actions(b.runner.state());
    assert!(
        actions.iter().any(|a| matches!(
            a,
            GameAction::CastSpell { object_id, .. } if *object_id == b.spell
        )),
        "Firestorm with {{R}} available must be castable; legal actions: {actions:?}"
    );
}

#[test]
fn firestorm_discards_x_and_deals_x_to_each_of_x_targets() {
    let mut b = board();
    let life_before = b.runner.state().players[1].life;
    let discarded = [b.hand_cards[0], b.hand_cards[1], b.hand_cards[2]];
    let outcome = b
        .runner
        .cast(b.spell)
        .x(3)
        .target_objects(&b.creatures)
        .target_players(&[P1])
        .pay_cost_with(&discarded)
        .resolve();

    // CR 701.9a: X = 3 cards discarded as the cost.
    outcome.assert_zone(&discarded, Zone::Graveyard);
    // CR 120.3 + CR 704.5g: 3 damage to each 3/3 → both die; the player loses 3.
    outcome.assert_zone(&b.creatures, Zone::Graveyard);
    assert_eq!(outcome.state().players[1].life, life_before - 3);
    outcome.assert_zone(&[b.spell], Zone::Graveyard);
}
