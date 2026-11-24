//! Runtime regression for #9844 — Fertile Imagination creates two Saprolings
//! per revealed card of the chosen type.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FERTILE_IMAGINATION: &str = "Choose a card type. Target opponent reveals their hand. \
     Create two 1/1 green Saproling creature tokens for each card of the chosen type revealed this way. \
     (Artifact, battle, creature, enchantment, instant, kindred, land, planeswalker, and sorcery are card types.)";

/// Casts Fertile Imagination at an opponent holding two creature cards and one
/// land card, choosing `card_type`, and returns P0's Saproling token count.
fn saprolings_after_choosing(card_type: &str) -> usize {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_to_hand(P1, "Revealed Bear", 2, 2);
    scenario.add_creature_to_hand(P1, "Revealed Elk", 3, 3);
    scenario.add_land_to_hand(P1, "Revealed Forest");
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Fertile Imagination", false, FERTILE_IMAGINATION)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_player(P1)
        .choose_option(card_type)
        .resolve();

    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "resolution must complete, got {:?}",
        outcome.final_waiting_for()
    );
    assert!(outcome.state().stack.is_empty());
    outcome.assert_zone(&[spell], Zone::Graveyard);

    outcome
        .state()
        .objects
        .values()
        .filter(|object| {
            object.is_token
                && object.controller == P0
                && object.zone == Zone::Battlefield
                && object.card_types.subtypes.iter().any(|s| s == "Saproling")
        })
        .count()
}

/// CR 608.2c + CR 111.1: "two … tokens for each card of the chosen type
/// revealed this way" creates two tokens per matching revealed card.
#[test]
fn fertile_imagination_creates_two_tokens_per_chosen_type_card() {
    assert_eq!(saprolings_after_choosing("Creature"), 4);
    assert_eq!(saprolings_after_choosing("Land"), 2);
    assert_eq!(saprolings_after_choosing("Artifact"), 0);
}
