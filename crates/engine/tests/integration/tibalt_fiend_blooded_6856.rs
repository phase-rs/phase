//! Tibalt, the Fiend-Blooded [-4] must prompt for a target player and deal
//! damage equal to that player's hand size (issue #6856 review follow-up).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

// Verbatim [-4] line (Scryfall, Tibalt, the Fiend-Blooded).
const TIBALT_MINUS_FOUR: &str = "\u{2212}4: Tibalt deals damage equal to the number of cards in target player's hand to that player.";

#[test]
fn tibalt_minus_four_damages_targeted_player_for_their_hand_size() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_cards_in_hand(P1, &["Alpha", "Beta", "Gamma"]);
    // Name-only objects parse no ability (the t96 vacuum), so the planeswalker
    // is synthesized from its verbatim Oracle line like the Lukka witness.
    let tibalt = scenario
        .add_creature_from_oracle(P0, "Tibalt, the Fiend-Blooded", 0, 0, TIBALT_MINUS_FOUR)
        .id();
    let mut runner = scenario.build();
    {
        // Real planeswalker so the LOYALTY activation path (not the generic
        // activated-ability path) is the one under test.
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&tibalt).expect("tibalt");
        obj.card_types.core_types = vec![CoreType::Planeswalker];
        obj.base_card_types = obj.card_types.clone();
        obj.power = None;
        obj.toughness = None;
        obj.loyalty = Some(7);
        obj.counters.insert(CounterType::Loyalty, 7);
    }

    let outcome = runner.activate(tibalt, 0).target_player(P1).resolve();

    // CR 115.1 + CR 606.3 + CR 120.3: the announced player is dealt damage
    // equal to their hand size (3). Without the recipient rebind there is no
    // prompt and the event-context recipient resolves to nothing, so P1 stays
    // at 20 and this fails.
    let life = runner.state().players[P1.0 as usize].life;
    assert_eq!(
        life, 17,
        "Tibalt -4 must deal P1's hand size in damage, P1 life = {life}"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "no further prompt after resolution, got {:?}",
        outcome.final_waiting_for()
    );
}
