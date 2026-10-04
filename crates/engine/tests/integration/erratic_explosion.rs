//! CR 701.20a + CR 608.2c: Erratic Explosion — "Choose any target. Reveal cards
//! from the top of your library until you reveal a nonland card. Erratic
//! Explosion deals damage equal to that card's mana value to that permanent or
//! player. Put the revealed cards on the bottom of your library in any order."
//!
//! The revealed nonland card is NOT put into a hand: damage is dealt first, then
//! every revealed card is arranged onto the library bottom in the order chosen.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

const ERRATIC_EXPLOSION: &str = "Choose any target. Reveal cards from the top of your library until you reveal a nonland card. Erratic Explosion deals damage equal to that card's mana value to that permanent or player. Put the revealed cards on the bottom of your library in any order.";

#[test]
fn damage_is_dealt_then_the_revealed_cards_are_arranged_and_none_goes_to_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Erratic Explosion", false, ERRATIC_EXPLOSION)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
    let deep = scenario.add_card_to_library_top(P0, "Deep Card");
    let hit = scenario
        .add_spell_to_library_top(P0, "Three Drop", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let land2 = scenario
        .add_spell_to_library_top(P0, "Island", false)
        .as_land()
        .id();
    let land1 = scenario
        .add_spell_to_library_top(P0, "Forest", false)
        .as_land()
        .id();
    let mut runner = scenario.build();
    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    let life_before = runner.state().players[P1.0 as usize].life;

    let mut committed = runner.cast(spell).target_player(P1).commit();
    committed.act(GameAction::PassPriority).unwrap();
    committed.act(GameAction::PassPriority).unwrap();

    let WaitingFor::EffectZoneChoice { cards, .. } = committed.state().waiting_for.clone() else {
        panic!(
            "expected the any-order prompt for the three revealed cards, got {:?}",
            committed.state().waiting_for
        );
    };
    assert_eq!(
        cards
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        [land1, land2, hit].into_iter().collect(),
        "every revealed card is offered for arrangement"
    );
    // Damage happened before the arrangement was asked for.
    assert_eq!(
        committed.state().players[P1.0 as usize].life,
        life_before - 3,
        "damage equals the revealed card's mana value"
    );

    let submitted = vec![hit, land1, land2];
    committed
        .act(GameAction::SelectCards {
            cards: submitted.clone(),
        })
        .expect("the arrangement is accepted");

    let state = committed.state();
    assert_eq!(
        state.players[P0.0 as usize].hand.len(),
        hand_before - 1,
        "only the cast spell left the hand; no revealed card entered it"
    );
    let library: Vec<ObjectId> = state.players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    let mut expected = vec![deep];
    expected.extend(submitted);
    assert_eq!(library, expected, "the bottom follows the submitted order");
}
